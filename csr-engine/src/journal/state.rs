//! Server state, the **cluster feed** the live routes render, and the
//! board's view models.
//!
//! # What the live server renders (codex X4 finding 2)
//!
//! The production feed is [`crate::storage::dream_clusters::load_dream_clusters`]
//! — the consequence-cluster feed, not the legacy per-item list. A user of
//! the running server therefore sees:
//!
//! * the three **evidence-maturity columns** ([`BoardColumn`]) with their
//!   gates enforced in [`BoardColumn::classify`];
//! * the off-board **Unexamined** lane — open items the pass concluded
//!   *nothing* about, which may never appear in a column;
//! * the **Settled** and **Archive** partitions, each carrying the receipt
//!   that put it there.
//!
//! Ordering inside a column is [`crate::storage::dream_clusters::ClusterRank`]
//! and nothing else: five explicit tiers plus a determinism backstop. Churn
//! is not a field of a cluster at all, and the project name is used only for
//! the sidebar's navigation list — neither can reach the comparison.
//! `board_order_is_the_five_tiers_and_nothing_else` and
//! `live_board_order_survives_shuffled_input_churn_and_project_names` pin
//! that through the rendered page.
//!
//! # Honesty contract carried into the views
//!
//! * Counts are counts of rows actually loaded. Nothing is derived from the
//!   absence of evidence — an empty feed renders an explicit "nothing on
//!   record" state.
//! * `witnessed` / `receipt` / `age` are `Option`: a missing one drops the
//!   clause, it never becomes a placeholder.
//! * A read **failure** is never rendered as absence. [`DreamFeed::load`],
//!   [`DreamFeed::load_board`] and [`DreamFeed::detail_context`] all return
//!   `Result`, and the routes turn `Err` into an explicit degraded page
//!   (codex X4 finding 9).
//! * The last-pass label is `None` until the ledger actually has an event.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::astdiff::{self, AstDiffCache, AstDiffOutcome, AstDiffRequest, SymbolNode};
use super::composer::{self, Brief, CopyBlock, DreamSpend, StoredPlan, PLAN_LABEL};
use crate::dream::report as projection;
use crate::storage::dream_clusters::{
    self, ClusterPartition, DreamCluster, DreamClusterFeed, OpenItem, VerdictClass,
};
use crate::storage::dream_items::{self, ChurnTile, DreamItem, DreamItemGrade};
use crate::storage::queries;
use crate::storage::Storage;

/// Default page size for `/api/dreams`.
pub const DEFAULT_PAGE_LIMIT: usize = 20;
/// Hard ceiling on a caller-supplied `limit`, so a hostile-looking query
/// string cannot make the server render an unbounded page.
pub const MAX_PAGE_LIMIT: usize = 50;

/// Display cap per board column. `ColumnView::count` stays the true total.
pub const MAX_COLUMN_CARDS: usize = 40;
/// Display cap on the off-board Unexamined lane. `UnexaminedView::count`
/// stays the true total.
pub const MAX_UNEXAMINED_ROWS: usize = 40;
/// Display cap on each ledger partition. The `*_total` fields stay true.
pub const MAX_LEDGER_ROWS: usize = 40;

/// The origin tag every write this surface makes carries (locked decision 4).
pub const JOURNAL_ORIGIN: &str = "journal_ui";

// --- board columns (P2b) -----------------------------------------------------

/// The three evidence-maturity columns, left→right by **descending
/// confidence**. Declaration order is render order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum BoardColumn {
    /// Confirmed by evidence: a plan survived propose-verify.
    Proposals,
    /// Ground moved; not yet bound to open work. Reinstatements live here.
    Observations,
    /// Item-grade binding + adverse verdict + receipt.
    OutdatedClaims,
}

impl BoardColumn {
    pub const ALL: [BoardColumn; 3] = [
        BoardColumn::Proposals,
        BoardColumn::Observations,
        BoardColumn::OutdatedClaims,
    ];

    pub fn slug(&self) -> &'static str {
        match self {
            BoardColumn::Proposals => "proposals",
            BoardColumn::Observations => "observations",
            BoardColumn::OutdatedClaims => "outdated-claims",
        }
    }

    pub fn title(&self) -> &'static str {
        match self {
            BoardColumn::Proposals => "Proposals",
            BoardColumn::Observations => "Observations",
            BoardColumn::OutdatedClaims => "Outdated claims",
        }
    }

    /// The gate, stated on the page so a reader can check the column against
    /// the evidence rather than trusting the placement.
    pub fn gate(&self) -> &'static str {
        match self {
            BoardColumn::Proposals => {
                "Gate: a drafted plan survived propose-verify — every step traces to a stored \
                 row, a verbatim quote or a receipt."
            }
            BoardColumn::Observations => {
                "Gate: any stored verdict, including session-grade. Nothing here is claimed to \
                 bind to your open item; reinstatements sit here because they are restorations, \
                 not staleness."
            }
            BoardColumn::OutdatedClaims => {
                "Gate: item-grade binding + an adverse verdict (superseded_by / anchor_obsolete) \
                 + a receipt oid. All three, or it is not in this column."
            }
        }
    }

    /// What an empty column means. Never "all clear".
    pub fn empty_note(&self) -> &'static str {
        match self {
            BoardColumn::Proposals => {
                "No plan has survived propose-verify. That is a statement about the plan pass, \
                 not about your work."
            }
            BoardColumn::Observations => {
                "No stored verdict is unbound. This is an empty column, not an all-clear."
            }
            BoardColumn::OutdatedClaims => {
                "No open item is provably stale: nothing cleared item-grade binding + adverse \
                 verdict + receipt together."
            }
        }
    }

    /// Classify one **active** cluster. `has_verified_plan` is the only input
    /// that is not on the cluster itself; it is `true` only when a stored
    /// `dream_plans` row for one of the cluster's items kept ≥1 step through
    /// the verifier.
    ///
    /// Gates are evaluated in descending-confidence order, so a cluster that
    /// clears more than one lands in the leftmost it clears.
    pub fn classify(cluster: &DreamCluster, has_verified_plan: bool) -> BoardColumn {
        if has_verified_plan {
            return BoardColumn::Proposals;
        }
        // Outdated claims needs ALL THREE: item-grade binding, an adverse
        // verdict, and a receipt. `ReceiptBearingAdverse` is exactly
        // "≥1 anchor_obsolete/superseded_by row that carries a receipt oid",
        // so the conjunction below is the gate verbatim.
        if cluster.grade == DreamItemGrade::ItemGrade
            && cluster.verdict_class == VerdictClass::ReceiptBearingAdverse
        {
            return BoardColumn::OutdatedClaims;
        }
        BoardColumn::Observations
    }
}

// --- write path (locked decision 4) ------------------------------------------

/// The only two writes this surface can make.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalAction {
    /// The user asserts the open item is done.
    Resolve,
    /// The user judges the dream conclusion not actionable. This is **not**
    /// a resolution and is never recorded as one.
    Dismiss,
}

impl JournalAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            JournalAction::Resolve => "resolve",
            JournalAction::Dismiss => "dismiss",
        }
    }

    /// The `resolution_ledger` status. Dismiss records `still_open` on
    /// purpose: dismissing a dream says nothing about the item's state.
    pub fn status(&self) -> &'static str {
        match self {
            JournalAction::Resolve => "resolved",
            JournalAction::Dismiss => "still_open",
        }
    }

    /// The evidence sentence stored with the verdict. It names the origin and
    /// says plainly that a human, not a check, made the call.
    pub fn evidence_for(&self, item: &DreamItem) -> String {
        match self {
            JournalAction::Resolve => format!(
                "Marked resolved from the CSR dream journal UI (origin {JOURNAL_ORIGIN}) on the \
                 open item {:?}, left open in session {}. A person asserted this; no automated \
                 check verified it.",
                item.item, item.origin_session
            ),
            JournalAction::Dismiss => format!(
                "Dismissed from the CSR dream journal UI (origin {JOURNAL_ORIGIN}): a person \
                 judged the dream conclusion for {:?} not actionable. Recorded as still open — \
                 a dismissal is not a resolution.",
                item.item
            ),
        }
    }
}

/// What a completed write actually did. Every field is measured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolveReceipt {
    pub action: &'static str,
    pub item_id: String,
    pub item: String,
    pub project: String,
    pub origin_session: String,
    pub status: &'static str,
    /// Number of `resolution_ledger` rows written — one per chunk of the
    /// origin conversation.
    pub chunks: usize,
    pub origin: &'static str,
}

/// Why a write did not happen. Never rendered as a success, never silently
/// swallowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// The feed cannot write at all (fixture feeds, read-only hosts).
    Unsupported,
    /// The item's origin conversation has no embedded chunk, so there is no
    /// row a verdict could attach to. Reported, never faked.
    NoChunks {
        session: String,
    },
    Storage(String),
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveError::Unsupported => {
                write!(f, "this journal feed records no verdicts")
            }
            ResolveError::NoChunks { session } => write!(
                f,
                "no embedded chunk exists for session {session}, so there is nothing to record a \
                 verdict against. Nothing was written."
            ),
            ResolveError::Storage(reason) => write!(f, "the verdict write failed: {reason}"),
        }
    }
}

// --- CSRF ---------------------------------------------------------------------

/// Per-process key behind the per-render CSRF token.
///
/// The token is `sha256(key ‖ 0x1f ‖ item_id)`, so it is bound to the exact
/// item the form targets and cannot be minted without the key. The key is
/// random per process and never leaves it; the pages are same-origin-only and
/// set no CORS header, so a cross-origin document cannot read a rendered
/// token either.
pub struct CsrfKey([u8; 32]);

impl CsrfKey {
    pub fn random() -> Self {
        let mut key = [0u8; 32];
        key[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        key[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        Self(key)
    }

    pub fn token(&self, item_id: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.0);
        hasher.update([0x1f]);
        hasher.update(item_id.as_bytes());
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// Constant-time comparison — a token check must not leak its prefix
    /// through timing.
    pub fn verify(&self, item_id: &str, presented: &str) -> bool {
        let expected = self.token(item_id);
        if expected.len() != presented.len() {
            return false;
        }
        let mut diff = 0u8;
        for (a, b) in expected.bytes().zip(presented.bytes()) {
            diff |= a ^ b;
        }
        diff == 0
    }
}

impl std::fmt::Debug for CsrfKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the key.
        f.write_str("CsrfKey(<redacted>)")
    }
}

// --- feed ---------------------------------------------------------------------

/// Everything the board needs from storage in one read.
#[derive(Debug, Clone, Default)]
pub struct BoardFeed {
    pub clusters: DreamClusterFeed,
    /// Every open item, tagged with whether the gate matched anything.
    pub open_items: Vec<OpenItem>,
    /// Item ids whose stored plan kept ≥1 step through the verifier. The
    /// Proposals gate reads this and nothing else.
    pub verified_plan_items: BTreeSet<String>,
}

/// The feed behind the journal.
///
/// Implementations run **blocking** SQLite work — routes call them inside
/// `spawn_blocking`, never on a runtime thread.
pub trait DreamFeed: Send + Sync + 'static {
    /// The per-item feed. Detail pages and write targets are validated
    /// against it; the board is not built from it.
    fn load(&self) -> Result<Vec<DreamItem>>;

    /// The cluster feed the landing page renders. No default: a feed that
    /// cannot answer must say so, not return an empty board that reads as
    /// "nothing on record".
    fn load_board(&self) -> Result<BoardFeed>;

    /// `(observed_head_oid, created_at)` of the newest ledger event, or
    /// `None` when nothing has ever been witnessed.
    fn last_pass(&self) -> Option<(String, String)> {
        None
    }

    /// Everything the detail page composes from stored rows.
    ///
    /// Returns `Result` (codex X4 finding 9): a failed read must reach the
    /// route as an error and render a degraded page. The default is
    /// `Ok(DetailContext::default())` — a *successful* read of a fixture that
    /// genuinely stores nothing, which is the only way an empty context may
    /// arise.
    fn detail_context(&self, _item: &DreamItem) -> Result<DetailContext> {
        Ok(DetailContext::default())
    }

    /// Record a resolve/dismiss verdict. `item` is a row that came out of
    /// [`DreamFeed::load`] — never a caller-supplied id.
    fn record_verdict(
        &self,
        _item: &DreamItem,
        _action: JournalAction,
    ) -> std::result::Result<ResolveReceipt, ResolveError> {
        Err(ResolveError::Unsupported)
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

/// Production feed: the cluster feed over the shared `Storage` connection,
/// plus the P4 composer feeds and the P2b gates.
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

/// The append-only audit table this surface writes alongside every verdict.
///
/// Created lazily here rather than in `storage::migrations` so the write path
/// owns its own record: the journal is the only writer and the only reader.
/// `CREATE TABLE IF NOT EXISTS` is idempotent and runs inside the same
/// connection as the insert.
pub(crate) fn ensure_journal_audit(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS journal_audit (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            action TEXT NOT NULL,
            item_id TEXT NOT NULL,
            project TEXT NOT NULL,
            origin_session TEXT NOT NULL,
            status TEXT NOT NULL,
            chunk_count INTEGER NOT NULL,
            origin TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
         );",
    )?;
    Ok(())
}

impl DreamFeed for StorageDreamFeed {
    fn load(&self) -> Result<Vec<DreamItem>> {
        self.storage.with_connection(dream_items::load_dream_items)
    }

    fn load_board(&self) -> Result<BoardFeed> {
        self.storage.with_connection(|conn| {
            let clusters = dream_clusters::load_dream_clusters(conn, None)?;
            let open_items = dream_clusters::load_open_items(conn)?;
            // The Proposals gate: only plans whose verifier kept ≥1 step.
            // A sentinel row (the pass ran and nothing traced) is stored with
            // an empty steps array and must NOT promote anything.
            let mut stmt = conn.prepare(
                "SELECT DISTINCT item_id FROM dream_plans
                 WHERE json_valid(steps_json) AND json_array_length(steps_json) > 0",
            )?;
            let verified_plan_items = stmt
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<BTreeSet<String>>>()?;
            Ok(BoardFeed {
                clusters,
                open_items,
                verified_plan_items,
            })
        })
    }

    fn last_pass(&self) -> Option<(String, String)> {
        self.storage.last_dream_run().ok().flatten()
    }

    fn detail_context(&self, item: &DreamItem) -> Result<DetailContext> {
        // A failure here propagates. It must NOT collapse into an empty
        // context: "no request, no completion, no night thread on record" is
        // a claim about the corpus, and a locked database has proved no such
        // thing (codex X4 finding 9).
        let (episode, threads, plan, spend, mut churn) = self.storage.with_connection(|conn| {
            let episode = composer::episode_facts(conn, &item.origin_session)?;
            let threads = composer::threads_for_session(conn, &item.origin_session)?;
            let plan = composer::load_plan(conn, &item.id)?;
            let refs = composer::spend_refs(&threads, plan.as_ref());
            let spend = composer::load_spend(conn, &refs)?;
            let churn = dream_items::load_churn(conn, &item.project, CHURN_WINDOW_DAYS)?;
            Ok((episode, threads, plan, spend, churn))
        })?;
        churn.truncate(CHURN_MAX_ROWS);

        let (ast, ast_abstention) = self.ast_slot(item);
        Ok(DetailContext {
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
        })
    }

    fn record_verdict(
        &self,
        item: &DreamItem,
        action: JournalAction,
    ) -> std::result::Result<ResolveReceipt, ResolveError> {
        // The verdict attaches to the chunks of the conversation the item was
        // left open in — real stored rows, looked up from the validated item.
        let chunk_ids = self
            .storage
            .get_chunk_ids_for_conversation(&item.origin_session)
            .map_err(|e| ResolveError::Storage(e.to_string()))?;
        if chunk_ids.is_empty() {
            return Err(ResolveError::NoChunks {
                session: item.origin_session.clone(),
            });
        }
        let status = action.status();
        let evidence = action.evidence_for(item);
        let written = self
            .storage
            .with_connection(|conn| {
                let written = queries::insert_resolutions(
                    conn,
                    &chunk_ids,
                    status,
                    &evidence,
                    Some(&item.item),
                    JOURNAL_ORIGIN,
                )?;
                ensure_journal_audit(conn)?;
                conn.execute(
                    "INSERT INTO journal_audit
                        (action, item_id, project, origin_session, status, chunk_count, origin)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    rusqlite::params![
                        action.as_str(),
                        item.id,
                        item.project,
                        item.origin_session,
                        status,
                        written as i64,
                        JOURNAL_ORIGIN,
                    ],
                )?;
                Ok(written)
            })
            .map_err(|e| ResolveError::Storage(e.to_string()))?;

        Ok(ResolveReceipt {
            action: action.as_str(),
            item_id: item.id.clone(),
            item: item.item.clone(),
            project: item.project.clone(),
            origin_session: item.origin_session.clone(),
            status,
            chunks: written,
            origin: JOURNAL_ORIGIN,
        })
    }
}

/// A fixed feed. Used by the route and template tests (no database, no
/// socket).
pub struct StaticDreamFeed {
    items: Vec<DreamItem>,
    board: BoardFeed,
    last_pass: Option<(String, String)>,
    context: Option<DetailContext>,
    /// When set, `detail_context` reports this failure instead of a context —
    /// how the degraded-detail path is exercised without a broken database.
    detail_error: Option<String>,
    /// Records every accepted write, so the route tests can assert that a
    /// rejected request wrote nothing.
    writes: std::sync::Mutex<Vec<ResolveReceipt>>,
    writable: bool,
}

impl StaticDreamFeed {
    pub fn new(items: Vec<DreamItem>) -> Self {
        Self {
            items,
            board: BoardFeed::default(),
            last_pass: None,
            context: None,
            detail_error: None,
            writes: std::sync::Mutex::new(Vec::new()),
            writable: false,
        }
    }

    pub fn with_board(mut self, board: BoardFeed) -> Self {
        self.board = board;
        self
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

    /// Make `detail_context` fail, the way a corrupt row or a locked database
    /// makes the production one fail.
    pub fn with_detail_error(mut self, reason: &str) -> Self {
        self.detail_error = Some(reason.to_string());
        self
    }

    /// Accept writes and log them in memory.
    pub fn writable(mut self) -> Self {
        self.writable = true;
        self
    }

    pub fn writes(&self) -> Vec<ResolveReceipt> {
        self.writes.lock().expect("writes lock").clone()
    }
}

impl DreamFeed for StaticDreamFeed {
    fn load(&self) -> Result<Vec<DreamItem>> {
        Ok(self.items.clone())
    }

    fn load_board(&self) -> Result<BoardFeed> {
        Ok(self.board.clone())
    }

    fn last_pass(&self) -> Option<(String, String)> {
        self.last_pass.clone()
    }

    fn detail_context(&self, _item: &DreamItem) -> Result<DetailContext> {
        if let Some(reason) = &self.detail_error {
            return Err(anyhow::anyhow!("{reason}"));
        }
        Ok(self.context.clone().unwrap_or_default())
    }

    fn record_verdict(
        &self,
        item: &DreamItem,
        action: JournalAction,
    ) -> std::result::Result<ResolveReceipt, ResolveError> {
        if !self.writable {
            return Err(ResolveError::Unsupported);
        }
        let receipt = ResolveReceipt {
            action: action.as_str(),
            item_id: item.id.clone(),
            item: item.item.clone(),
            project: item.project.clone(),
            origin_session: item.origin_session.clone(),
            status: action.status(),
            chunks: 1,
            origin: JOURNAL_ORIGIN,
        };
        self.writes
            .lock()
            .expect("writes lock")
            .push(receipt.clone());
        Ok(receipt)
    }
}

/// Shared, cheaply-cloned server state.
#[derive(Clone)]
pub struct JournalState {
    feed: Arc<dyn DreamFeed>,
    csrf: Arc<CsrfKey>,
}

impl JournalState {
    pub fn new(feed: Arc<dyn DreamFeed>) -> Self {
        Self {
            feed,
            csrf: Arc::new(CsrfKey::random()),
        }
    }

    /// Convenience constructor for the production wiring.
    pub fn from_storage(storage: Arc<Storage>) -> Self {
        Self::new(Arc::new(StorageDreamFeed::new(storage)))
    }

    pub fn feed(&self) -> Arc<dyn DreamFeed> {
        self.feed.clone()
    }

    pub fn csrf(&self) -> Arc<CsrfKey> {
        self.csrf.clone()
    }
}

// --- view models -------------------------------------------------------------

/// Per-dream spend as rendered (locked decision 13). Built **only** from a
/// measured [`DreamSpend`]; there is no constructor that produces a zeroed
/// one, so a detail page with no recorded usage carries `spend: None` and the
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

/// One three-line index card. Sparse by design (P6): verdict + grade caps,
/// the conclusion sentence, and one mono line of `age · receipt · affected`.
/// Everything else — plan steps, spend, affected-item rows, absolute dates —
/// lives on the detail page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClusterCardView {
    pub id: String,
    pub project: String,
    pub column: &'static str,
    /// `01`, `02`, … within the column. `None` for a card excluded from
    /// ranking (see `not_live`).
    pub ordinal: Option<String>,
    /// Verbatim verdict, spaces for underscores: `"anchor obsolete"`.
    pub verdict: String,
    /// `receipt-bearing` / `restorative` / `unreceipted`.
    pub verdict_class: &'static str,
    /// Which semantic hue the 3px rail may use. Derived from the verdict
    /// alone, so no other dimension (priority, project, spend) can borrow a
    /// verdict colour.
    pub hue: &'static str,
    pub grade_slug: &'static str,
    pub grade_label: &'static str,
    /// Line 2 — what the ground-shift was, read off stored fields only.
    pub conclusion: String,
    /// Line 3 — `age · ⌗receipt · N affected`. Clauses drop independently.
    pub meta: String,
    /// Link to the lead item's detail page. `None` when the cluster carries
    /// no item at all, in which case no link is rendered.
    pub href: Option<String>,
    /// Where the micro copy icon points: the detail page's copy block.
    pub copy_href: Option<String>,
    /// `true` for a cluster with no receipt-bearing verdict. Rendered
    /// neutral + dashed + `NOT LIVE`, and never given a rank numeral.
    pub not_live: bool,
    /// Measured count of items this conclusion affects.
    pub affected: usize,
}

/// One board column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ColumnView {
    pub slug: &'static str,
    pub title: &'static str,
    pub gate: &'static str,
    pub empty_note: &'static str,
    pub cards: Vec<ClusterCardView>,
    /// TRUE total for the column; may exceed `cards.len()`.
    pub count: usize,
    /// `"showing 40 of 57"` when the column was display-capped.
    pub truncation: Option<String>,
}

/// One off-board Unexamined row. Deliberately carries no verdict, no
/// receipt and no link: the pass concluded nothing about it, so there is no
/// dream page to open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnexaminedRowView {
    pub item: String,
    pub kind: String,
    pub project: String,
    pub meta: String,
}

/// The off-board Unexamined lane.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct UnexaminedView {
    pub rows: Vec<UnexaminedRowView>,
    pub count: usize,
    pub truncation: Option<String>,
}

/// The sentence that keeps the Unexamined lane from reading as a verdict.
pub const UNEXAMINED_NOTE: &str =
    "Open items no verdict evidence matched. The night pass concluded NOTHING about these — \
     they are off the board on purpose, because placing them in a column would imply a \
     conclusion that does not exist.";

/// One settled/archived ledger row: compact, neutral, live actions removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LedgerRowView {
    pub id: String,
    pub project: String,
    /// The ORIGINAL verdict, as an outline label — not a live one.
    pub verdict: String,
    pub conclusion: String,
    /// `"completed 2026-08-09 · session 3f2a…"` — the proof it is settled.
    /// `None` on an archive row.
    pub completed: Option<String>,
    /// `"original witness 2026-08-01 · ⌗abcdef12"`. The receipt clause drops
    /// when no oid is stored.
    pub original_witness: String,
    /// `"pass 2026-08-05 · HEAD ⌗deadbeef · complete"`, or `None` when no
    /// generation manifest covers the evidence. Never invented.
    pub pass: Option<String>,
    pub href: Option<String>,
}

/// The landing page's full model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BoardView {
    /// Exactly three, in descending-confidence order.
    pub columns: Vec<ColumnView>,
    pub unexamined: UnexaminedView,
    pub unexamined_note: &'static str,
    pub settled: Vec<LedgerRowView>,
    pub settled_total: usize,
    /// `"showing 40 of 57"` when the settled ledger was display-capped.
    pub settled_truncation: Option<String>,
    pub archive: Vec<LedgerRowView>,
    pub archive_total: usize,
    pub archive_truncation: Option<String>,
    /// Every project with ≥1 cluster. Navigation only — never a rank input.
    pub projects: Vec<String>,
    /// `"4 conclusions · 12 open items"` — measured counts.
    pub count_label: String,
    /// `"last pass <date> · HEAD <oid8>"`, or `None`.
    pub pass_label: Option<String>,
    /// `true` only when every partition and the unexamined lane are empty.
    pub empty: bool,
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
    /// Per-render CSRF token for the resolve/dismiss forms, bound to `id`.
    pub csrf: String,
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

/// One page of the JSON API — the ranked ACTIVE clusters, flattened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DreamsPage {
    pub items: Vec<ClusterCardView>,
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
pub fn parse_cursor(cursor: Option<&str>) -> std::result::Result<usize, BadCursor> {
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

// --- projection helpers -------------------------------------------------------

/// Parse the three timestamp shapes the corpus stores. `None` for anything
/// else — an unparseable timestamp drops its clause rather than becoming a
/// wrong one.
fn parse_ts(raw: &str) -> Option<DateTime<Utc>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
        return Some(dt.with_timezone(&Utc));
    }
    if let Ok(naive) = NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S") {
        return Some(Utc.from_utc_datetime(&naive));
    }
    if let Ok(date) = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
        return Some(Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0)?));
    }
    None
}

/// Relative age for the index card's mono line. `None` when the timestamp
/// does not parse — the clause is dropped, never guessed.
pub fn age_label(raw: &str, now: DateTime<Utc>) -> Option<String> {
    let then = parse_ts(raw)?;
    let days = (now - then).num_days();
    Some(match days {
        d if d <= 0 => "today".to_string(),
        1 => "1d ago".to_string(),
        d if d < 60 => format!("{d}d ago"),
        d if d < 730 => format!("{}mo ago", d / 30),
        d => format!("{}y ago", d / 365),
    })
}

/// Line 2 of an index card: what moved, from stored fields only.
fn conclusion_sentence(cluster: &DreamCluster) -> String {
    let conclusion = &cluster.conclusion;
    if conclusion.verdict.trim().is_empty() && conclusion.file.trim().is_empty() {
        return "No verdict row is stored for this conclusion, so nothing is claimed to have \
                changed."
            .to_string();
    }
    let subject = match &conclusion.symbol {
        Some(symbol) => format!("{symbol} in {}", conclusion.file),
        None => conclusion.file.clone(),
    };
    format!(
        "{subject} was witnessed {}.",
        conclusion.verdict.replace('_', " ")
    )
}

/// Line 3: `age · ⌗receipt · N affected`. Each clause is independent — a
/// missing receipt or an unparseable timestamp drops only itself.
fn card_meta(cluster: &DreamCluster, now: DateTime<Utc>) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(age) = age_label(&cluster.witnessed_at, now) {
        parts.push(age);
    }
    if let Some(oid) = &cluster.conclusion.receipt_oid {
        parts.push(format!("⌗{}", projection::short_oid(oid)));
    } else {
        parts.push("receipt unavailable".to_string());
    }
    parts.push(format!(
        "{} affected",
        projection::pluralize(cluster.counts.items, "item")
    ));
    parts.join(" · ")
}

/// The semantic hue a card's rail may use. Neutral unless the verdict itself
/// is receipt-bearing — an unreceipted row must never borrow amber/rose/sage,
/// because those assert that a deterministic verdict exists.
fn hue_for(cluster: &DreamCluster) -> &'static str {
    if cluster.conclusion.receipt_oid.is_none() {
        return "neutral";
    }
    match cluster.conclusion.verdict.as_str() {
        "anchor_obsolete" => "obsolete",
        "superseded_by" => "superseded",
        "anchor_reinstated" => "reinstated",
        _ => "neutral",
    }
}

/// Project one cluster into its index card.
pub fn build_cluster_card(
    cluster: &DreamCluster,
    column: BoardColumn,
    ordinal: Option<usize>,
    now: DateTime<Utc>,
) -> ClusterCardView {
    let (grade_slug, grade_label) = projection::dream_grade_slug_label(cluster.grade);
    let lead = cluster.items.first().map(|item| item.id.clone());
    let not_live = cluster.verdict_class == VerdictClass::Unreceipted;
    ClusterCardView {
        id: cluster.id.clone(),
        project: cluster.project.clone(),
        column: column.slug(),
        ordinal: ordinal.filter(|_| !not_live).map(|n| format!("{n:02}")),
        verdict: cluster.conclusion.verdict.replace('_', " "),
        verdict_class: cluster.verdict_class.as_str(),
        hue: hue_for(cluster),
        grade_slug,
        grade_label,
        conclusion: conclusion_sentence(cluster),
        meta: card_meta(cluster, now),
        href: lead.as_deref().map(href_for),
        copy_href: lead.as_deref().map(|id| format!("/dream/{id}#copy")),
        not_live,
        affected: cluster.counts.items,
    }
}

fn ledger_row(cluster: &DreamCluster) -> LedgerRowView {
    let lead = cluster.items.first().map(|item| item.id.clone());
    let mut witness = format!("original witness {}", cluster.witnessed_date);
    if let Some(oid) = &cluster.conclusion.receipt_oid {
        witness.push_str(&format!(" · ⌗{}", projection::short_oid(oid)));
    }
    LedgerRowView {
        id: cluster.id.clone(),
        project: cluster.project.clone(),
        verdict: cluster.conclusion.verdict.replace('_', " "),
        conclusion: conclusion_sentence(cluster),
        completed: cluster.settled.as_ref().map(|receipt| {
            format!(
                "completed {} · session {}",
                receipt.completed_date,
                projection::short_oid(&receipt.session_id)
            )
        }),
        original_witness: witness,
        pass: cluster.archive_pass.as_ref().map(|pass| {
            format!(
                "pass {} · HEAD ⌗{} · {}",
                pass.created_date,
                projection::short_oid(&pass.head_oid),
                pass.status
            )
        }),
        href: lead.as_deref().map(href_for),
    }
}

/// The ACTIVE clusters, in rank order, each tagged with its column.
///
/// This is the whole ordering story: `sort_by(|a, b| a.rank.cmp(&b.rank))`.
/// The cluster type carries no churn field, and the project name is not part
/// of [`crate::storage::dream_clusters::ClusterRank`], so neither can
/// influence the result.
pub fn ranked_active(feed: &BoardFeed) -> Vec<(BoardColumn, DreamCluster)> {
    let mut active: Vec<DreamCluster> = feed
        .clusters
        .active
        .iter()
        .filter(|cluster| cluster.partition == ClusterPartition::Active)
        .cloned()
        .collect();
    active.sort_by(|a, b| a.rank.cmp(&b.rank));
    active
        .into_iter()
        .map(|cluster| {
            let planned = cluster
                .items
                .iter()
                .any(|item| feed.verified_plan_items.contains(&item.id));
            (BoardColumn::classify(&cluster, planned), cluster)
        })
        .collect()
}

/// Flatten the ranked active clusters into cards, for `/api/dreams`.
pub fn ranked_active_cards(feed: &BoardFeed, now: DateTime<Utc>) -> Vec<ClusterCardView> {
    ranked_active(feed)
        .iter()
        .enumerate()
        .map(|(index, (column, cluster))| {
            build_cluster_card(cluster, *column, Some(index + 1), now)
        })
        .collect()
}

fn truncation_note(shown: usize, total: usize) -> Option<String> {
    (total > shown).then(|| format!("showing {shown} of {total}"))
}

/// Build the whole board.
///
/// `now` is a parameter rather than a call to `Utc::now()` so the rendered
/// age labels are deterministic under test.
pub fn build_board(
    feed: &BoardFeed,
    last_pass: Option<(String, String)>,
    now: DateTime<Utc>,
) -> BoardView {
    let ranked = ranked_active(feed);

    let mut columns: Vec<ColumnView> = Vec::with_capacity(BoardColumn::ALL.len());
    for column in BoardColumn::ALL {
        let members: Vec<&DreamCluster> = ranked
            .iter()
            .filter(|(assigned, _)| *assigned == column)
            .map(|(_, cluster)| cluster)
            .collect();
        let count = members.len();
        let cards: Vec<ClusterCardView> = members
            .iter()
            .take(MAX_COLUMN_CARDS)
            .enumerate()
            .map(|(index, cluster)| build_cluster_card(cluster, column, Some(index + 1), now))
            .collect();
        columns.push(ColumnView {
            slug: column.slug(),
            title: column.title(),
            gate: column.gate(),
            empty_note: column.empty_note(),
            truncation: truncation_note(cards.len(), count),
            cards,
            count,
        });
    }

    // Unexamined: open, not completed later, and the gate matched nothing.
    // These may never appear in a column.
    let unexamined_all: Vec<&OpenItem> = feed
        .open_items
        .iter()
        .filter(|item| !item.examined && item.completed.is_none())
        .collect();
    let unexamined_count = unexamined_all.len();
    let unexamined_rows: Vec<UnexaminedRowView> = unexamined_all
        .iter()
        .take(MAX_UNEXAMINED_ROWS)
        .map(|item| UnexaminedRowView {
            item: item.item.clone(),
            kind: item.kind.clone(),
            project: item.project.clone(),
            meta: match age_label(&item.origin_ts, now) {
                Some(age) => format!("left open {age} · no verdict evidence"),
                None => "no verdict evidence".to_string(),
            },
        })
        .collect();

    let settled: Vec<LedgerRowView> = feed
        .clusters
        .settled
        .iter()
        .take(MAX_LEDGER_ROWS)
        .map(ledger_row)
        .collect();
    let archive: Vec<LedgerRowView> = feed
        .clusters
        .archive
        .iter()
        .take(MAX_LEDGER_ROWS)
        .map(ledger_row)
        .collect();

    let conclusions = ranked.len();
    let affected: usize = ranked.iter().map(|(_, cluster)| cluster.counts.items).sum();

    BoardView {
        empty: conclusions == 0
            && unexamined_count == 0
            && feed.clusters.total_settled == 0
            && feed.clusters.total_archive == 0,
        columns,
        unexamined: UnexaminedView {
            truncation: truncation_note(unexamined_rows.len(), unexamined_count),
            rows: unexamined_rows,
            count: unexamined_count,
        },
        unexamined_note: UNEXAMINED_NOTE,
        settled_truncation: truncation_note(settled.len(), feed.clusters.total_settled),
        settled,
        settled_total: feed.clusters.total_settled,
        archive_truncation: truncation_note(archive.len(), feed.clusters.total_archive),
        archive,
        archive_total: feed.clusters.total_archive,
        projects: feed.clusters.projects.clone(),
        count_label: format!(
            "{} · {}",
            projection::pluralize(conclusions, "conclusion"),
            projection::pluralize(affected, "affected item")
        ),
        pass_label: pass_label(last_pass),
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
/// sections attached. `csrf` is the per-render token for the two write forms.
pub fn build_detail(item: &DreamItem, context: &DetailContext, csrf: String) -> DetailView {
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
        csrf,
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

/// Slice the ranked active cards into one JSON API page.
pub fn build_page(cards: &[ClusterCardView], offset: usize, limit: usize) -> DreamsPage {
    let total = cards.len();
    let start = offset.min(total);
    let end = start.saturating_add(limit).min(total);
    DreamsPage {
        items: cards[start..end].to_vec(),
        next_cursor: (end < total).then(|| end.to_string()),
        total,
    }
}

/// Find one item by its stable id. The id is compared against ids that came
/// out of the feed — it is never interpolated into SQL, a path, or a shell
/// command, so an arbitrary `:id` in the URL can only ever 404, and a write
/// can only ever target a row that already exists.
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

/// A [`BoardFeed`] over a fixed cluster list, for the template and route
/// tests. Shared so the two suites cannot drift into different fixtures.
#[cfg(test)]
pub(crate) fn board_feed_of(clusters: Vec<DreamCluster>) -> BoardFeed {
    let projects: Vec<String> = clusters
        .iter()
        .map(|cluster| cluster.project.clone())
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect();
    BoardFeed {
        clusters: DreamClusterFeed {
            total_active: clusters.len(),
            active: clusters,
            settled: Vec::new(),
            archive: Vec::new(),
            total_settled: 0,
            total_archive: 0,
            projects,
        },
        open_items: Vec::new(),
        verified_plan_items: BTreeSet::new(),
    }
}

/// A fixed clock, so every rendered age label in the test suite is
/// deterministic.
#[cfg(test)]
pub(crate) fn test_now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-08-11T00:00:00Z")
        .expect("fixed clock")
        .with_timezone(&Utc)
}

/// One cluster, fully formed, for the board tests. Every knob a ranking tier
/// reads is a parameter, so a tier can be exercised in isolation.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn sample_cluster(
    id: &str,
    project: &str,
    grade: DreamItemGrade,
    verdict: &str,
    receipt: Option<&str>,
    witnessed_at: &str,
    kind: &str,
    oldest_open: &str,
    items: usize,
) -> DreamCluster {
    use crate::storage::dream_clusters::{
        ClusterConclusion, ClusterCounts, ClusterItem, ClusterRank, ItemKindRank,
    };
    use crate::storage::dream_items::DreamEvidence;
    use std::cmp::Reverse;

    let evidence: Vec<DreamEvidence> = vec![DreamEvidence {
        symbol: Some("run_report".to_string()),
        file: "csr-engine/src/dream/report.rs".to_string(),
        verdict: verdict.to_string(),
        receipt_oid: receipt.map(str::to_string),
        witnessed_at: witnessed_at.to_string(),
    }];
    let verdict_class =
        if matches!(verdict, "anchor_obsolete" | "superseded_by") && receipt.is_some() {
            VerdictClass::ReceiptBearingAdverse
        } else if verdict == "anchor_reinstated" {
            VerdictClass::Restorative
        } else {
            VerdictClass::Unreceipted
        };
    let cluster_items: Vec<ClusterItem> = (0..items)
        .map(|index| ClusterItem {
            id: format!("{id}-item{index}"),
            item: format!("open item {index} of {id}"),
            kind: kind.to_string(),
            origin_session: format!("sess-{id}"),
            origin_ts: oldest_open.to_string(),
            origin_date: projection::iso_date(oldest_open),
            completed: None,
        })
        .collect();

    DreamCluster {
        id: id.to_string(),
        project: project.to_string(),
        origin_session: format!("sess-{id}"),
        fingerprint: format!("fp-{id}"),
        grade,
        standalone: grade == DreamItemGrade::ItemGrade,
        partition: ClusterPartition::Active,
        conclusion: ClusterConclusion {
            verdict: verdict.to_string(),
            verdict_class,
            symbol: Some("run_report".to_string()),
            file: "csr-engine/src/dream/report.rs".to_string(),
            receipt_oid: receipt.map(str::to_string),
            witnessed_at: witnessed_at.to_string(),
            witnessed_date: projection::iso_date(witnessed_at),
        },
        verdict_class,
        evidence,
        receipts: receipt.into_iter().map(str::to_string).collect(),
        witnessed_date: projection::iso_date(witnessed_at),
        witnessed_at: witnessed_at.to_string(),
        oldest_open_date: projection::iso_date(oldest_open),
        oldest_open_ts: oldest_open.to_string(),
        counts: ClusterCounts {
            items,
            blockers: if kind == "blocker" { items } else { 0 },
            todos: if kind == "todo" { items } else { 0 },
            completed_items: 0,
            evidence: 1,
            receipts: usize::from(receipt.is_some()),
            files: 1,
            symbols: 1,
        },
        items: cluster_items,
        settled: None,
        archive_pass: None,
        rank: ClusterRank {
            grade,
            verdict_class,
            witnessed_date_desc: Reverse(projection::iso_date(witnessed_at)),
            kind: if kind == "blocker" {
                ItemKindRank::Blocker
            } else {
                ItemKindRank::Todo
            },
            oldest_open: oldest_open
                .replace('T', " ")
                .trim_end_matches('Z')
                .to_string(),
            id: id.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cards(n: usize) -> Vec<ClusterCardView> {
        (0..n)
            .map(|i| {
                let cluster = sample_cluster(
                    &format!("c{i:02}"),
                    "proj",
                    DreamItemGrade::ItemGrade,
                    "anchor_obsolete",
                    Some("abcdef1234567890"),
                    "2026-08-09T12:00:00Z",
                    "todo",
                    "2026-08-01T09:00:00Z",
                    1,
                );
                build_cluster_card(
                    &cluster,
                    BoardColumn::OutdatedClaims,
                    Some(i + 1),
                    test_now(),
                )
            })
            .collect()
    }

    fn board_of(clusters: Vec<DreamCluster>) -> BoardFeed {
        board_feed_of(clusters)
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
        let all = cards(5);
        let first = build_page(&all, 0, 2);
        assert_eq!(first.items.len(), 2);
        assert_eq!(first.total, 5);
        assert_eq!(first.next_cursor.as_deref(), Some("2"));

        let second = build_page(&all, 2, 2);
        assert_eq!(second.items[0].id, "c02");
        assert_eq!(second.next_cursor.as_deref(), Some("4"));

        let last = build_page(&all, 4, 2);
        assert_eq!(last.items.len(), 1);
        assert_eq!(last.next_cursor, None, "last page must not advertise more");
    }

    #[test]
    fn build_page_past_the_end_is_empty_not_wrapped() {
        let all = cards(3);
        let page = build_page(&all, 99, 10);
        assert!(page.items.is_empty());
        assert_eq!(page.total, 3);
        assert_eq!(page.next_cursor, None);
    }

    // --- board gates ---------------------------------------------------------

    #[test]
    fn outdated_claims_needs_all_three_of_its_gate_conditions() {
        let full = sample_cluster(
            "a",
            "proj",
            DreamItemGrade::ItemGrade,
            "anchor_obsolete",
            Some("abcdef1234567890"),
            "2026-08-09T12:00:00Z",
            "todo",
            "2026-08-01T09:00:00Z",
            1,
        );
        assert_eq!(
            BoardColumn::classify(&full, false),
            BoardColumn::OutdatedClaims
        );

        // Drop the receipt → not provably stale.
        let no_receipt = sample_cluster(
            "a",
            "proj",
            DreamItemGrade::ItemGrade,
            "anchor_obsolete",
            None,
            "2026-08-09T12:00:00Z",
            "todo",
            "2026-08-01T09:00:00Z",
            1,
        );
        assert_eq!(
            BoardColumn::classify(&no_receipt, false),
            BoardColumn::Observations
        );

        // Drop the item-grade binding → the conclusion is not bound to the
        // item's own symbols, so it cannot be called stale work.
        let session = sample_cluster(
            "a",
            "proj",
            DreamItemGrade::SessionGrade,
            "anchor_obsolete",
            Some("abcdef1234567890"),
            "2026-08-09T12:00:00Z",
            "todo",
            "2026-08-01T09:00:00Z",
            1,
        );
        assert_eq!(
            BoardColumn::classify(&session, false),
            BoardColumn::Observations
        );

        // Restorative verdicts are Observations by directive, never stale.
        let reinstated = sample_cluster(
            "a",
            "proj",
            DreamItemGrade::ItemGrade,
            "anchor_reinstated",
            Some("abcdef1234567890"),
            "2026-08-09T12:00:00Z",
            "todo",
            "2026-08-01T09:00:00Z",
            1,
        );
        assert_eq!(
            BoardColumn::classify(&reinstated, false),
            BoardColumn::Observations
        );
    }

    #[test]
    fn proposals_requires_a_verified_plan_and_outranks_the_other_gates() {
        let cluster = sample_cluster(
            "a",
            "proj",
            DreamItemGrade::ItemGrade,
            "anchor_obsolete",
            Some("abcdef1234567890"),
            "2026-08-09T12:00:00Z",
            "todo",
            "2026-08-01T09:00:00Z",
            1,
        );
        assert_eq!(
            BoardColumn::classify(&cluster, false),
            BoardColumn::OutdatedClaims
        );
        assert_eq!(
            BoardColumn::classify(&cluster, true),
            BoardColumn::Proposals,
            "a verified plan is the highest-confidence gate"
        );
    }

    #[test]
    fn a_sentinel_plan_row_does_not_promote_anything() {
        // `verified_plan_items` is populated ONLY from rows whose steps array
        // is non-empty (the SQL in `StorageDreamFeed::load_board`). A cluster
        // whose item is absent from that set is classified by evidence alone.
        let cluster = sample_cluster(
            "a",
            "proj",
            DreamItemGrade::SessionGrade,
            "anchor_obsolete",
            None,
            "2026-08-09T12:00:00Z",
            "todo",
            "2026-08-01T09:00:00Z",
            1,
        );
        let feed = board_of(vec![cluster]);
        assert!(feed.verified_plan_items.is_empty());
        let board = build_board(&feed, None, test_now());
        assert_eq!(board.columns[0].count, 0, "Proposals must stay empty");
        assert_eq!(board.columns[1].count, 1);
    }

    // --- ranking -------------------------------------------------------------

    /// Every tier boundary, built so that ONLY that tier can decide, and fed
    /// in the exact reverse of the expected order.
    fn tier_fixture() -> Vec<DreamCluster> {
        vec![
            // 5: oldest open last
            sample_cluster(
                "t5",
                "zzz",
                DreamItemGrade::SessionGrade,
                "anchor_reinstated",
                None,
                "2026-08-01T00:00:00Z",
                "todo",
                "2026-07-30T00:00:00Z",
                9,
            ),
            // 4: blocker before todo (same grade/class/date, older open)
            sample_cluster(
                "t4",
                "yyy",
                DreamItemGrade::SessionGrade,
                "anchor_reinstated",
                None,
                "2026-08-01T00:00:00Z",
                "blocker",
                "2026-07-31T00:00:00Z",
                8,
            ),
            // 3: newer witnessed date
            sample_cluster(
                "t3",
                "xxx",
                DreamItemGrade::SessionGrade,
                "anchor_reinstated",
                None,
                "2026-08-05T00:00:00Z",
                "todo",
                "2026-07-31T00:00:00Z",
                7,
            ),
            // 2: receipt-bearing adverse before restorative
            sample_cluster(
                "t2",
                "www",
                DreamItemGrade::SessionGrade,
                "superseded_by",
                Some("beef000000000000"),
                "2026-08-05T00:00:00Z",
                "todo",
                "2026-07-31T00:00:00Z",
                6,
            ),
            // 1: item-grade before session-grade
            sample_cluster(
                "t1",
                "vvv",
                DreamItemGrade::ItemGrade,
                "superseded_by",
                Some("beef000000000000"),
                "2026-08-05T00:00:00Z",
                "todo",
                "2026-07-31T00:00:00Z",
                5,
            ),
        ]
    }

    #[test]
    fn board_order_is_the_five_tiers_and_nothing_else() {
        let feed = board_of(tier_fixture());
        let order: Vec<String> = ranked_active(&feed)
            .iter()
            .map(|(_, cluster)| cluster.id.clone())
            .collect();
        assert_eq!(order, vec!["t1", "t2", "t3", "t4", "t5"]);
    }

    /// The fixture is deliberately adversarial: the LAST cluster in rank
    /// order (`t5`) has the alphabetically-last project, the most affected
    /// items and the oldest evidence, and the FIRST (`t1`) has the fewest
    /// items. If item volume, project name or any activity proxy leaked into
    /// the comparison, this order would change.
    #[test]
    fn neither_project_name_nor_item_volume_can_move_a_card() {
        let mut renamed = tier_fixture();
        for (index, cluster) in renamed.iter_mut().enumerate() {
            cluster.project = format!("aaa-{index}");
            cluster.counts.items = 100 - index;
        }
        let feed = board_of(renamed);
        let order: Vec<String> = ranked_active(&feed)
            .iter()
            .map(|(_, cluster)| cluster.id.clone())
            .collect();
        assert_eq!(order, vec!["t1", "t2", "t3", "t4", "t5"]);
    }

    #[test]
    fn shuffled_input_produces_the_same_total_order() {
        let mut shuffled = tier_fixture();
        shuffled.rotate_left(3);
        shuffled.swap(0, 4);
        let feed = board_of(shuffled);
        let order: Vec<String> = ranked_active(&feed)
            .iter()
            .map(|(_, cluster)| cluster.id.clone())
            .collect();
        assert_eq!(order, vec!["t1", "t2", "t3", "t4", "t5"]);
    }

    /// The ranking type has no field a churn measurement could occupy, and
    /// the board sorts on that type alone. This asserts the structural fact
    /// so a later "just add a heat weight" edit fails a test.
    #[test]
    fn the_rank_type_exposes_only_the_five_tiers_plus_the_id_backstop() {
        let cluster = &tier_fixture()[0];
        let rank = &cluster.rank;
        // Naming every field forces a compile error if one is added.
        let dream_clusters::ClusterRank {
            grade: _,
            verdict_class: _,
            witnessed_date_desc: _,
            kind: _,
            oldest_open: _,
            id: _,
        } = rank.clone();
    }

    // --- unexamined lane -----------------------------------------------------

    fn open_item(id: &str, examined: bool, completed: bool) -> OpenItem {
        use crate::storage::dream_clusters::CompletionReceipt;
        OpenItem {
            id: id.to_string(),
            project: "proj".to_string(),
            item: format!("do {id}"),
            kind: "todo".to_string(),
            origin_session: "sess-1".to_string(),
            origin_ts: "2026-08-01T09:00:00Z".to_string(),
            origin_date: "2026-08-01".to_string(),
            completed: completed.then(|| CompletionReceipt {
                session_id: "sess-2".to_string(),
                completed_at: "2026-08-05T09:00:00Z".to_string(),
                completed_date: "2026-08-05".to_string(),
            }),
            examined,
        }
    }

    #[test]
    fn unexamined_holds_only_open_items_with_no_verdict_evidence() {
        let mut feed = board_of(tier_fixture());
        feed.open_items = vec![
            open_item("gated", true, false),
            open_item("done", false, true),
            open_item("bare", false, false),
        ];
        let board = build_board(&feed, None, test_now());
        assert_eq!(board.unexamined.count, 1);
        assert_eq!(board.unexamined.rows[0].item, "do bare");
        assert!(board.unexamined.rows[0]
            .meta
            .contains("no verdict evidence"));

        // And it is nowhere in a column.
        for column in &board.columns {
            for card in &column.cards {
                assert!(!card.conclusion.contains("do bare"));
                assert!(!card.id.contains("bare"));
            }
        }
    }

    #[test]
    fn an_empty_board_says_so_and_invents_nothing() {
        let board = build_board(&BoardFeed::default(), None, test_now());
        assert!(board.empty);
        assert_eq!(board.count_label, "0 conclusions · 0 affected items");
        assert_eq!(board.unexamined.count, 0);
        assert_eq!(board.pass_label, None);
        assert_eq!(board.columns.len(), 3);
        for column in &board.columns {
            assert!(column.cards.is_empty());
            assert!(!column.empty_note.to_lowercase().contains("all clear"));
        }
    }

    #[test]
    fn a_card_without_a_receipt_is_marked_not_live_and_gets_no_rank_numeral() {
        let cluster = sample_cluster(
            "u1",
            "proj",
            DreamItemGrade::ItemGrade,
            "anchor_obsolete",
            None,
            "2026-08-09T12:00:00Z",
            "todo",
            "2026-08-01T09:00:00Z",
            1,
        );
        let card = build_cluster_card(&cluster, BoardColumn::Observations, Some(1), test_now());
        assert!(card.not_live);
        assert_eq!(
            card.ordinal, None,
            "unverified evidence is excluded from ranking"
        );
        assert!(card.meta.contains("receipt unavailable"));
        assert!(
            !card.meta.contains("⌗"),
            "a missing receipt must never render a placeholder oid"
        );
    }

    #[test]
    fn card_meta_drops_the_age_clause_when_the_timestamp_does_not_parse() {
        let mut cluster = sample_cluster(
            "u1",
            "proj",
            DreamItemGrade::ItemGrade,
            "anchor_obsolete",
            Some("abcdef1234567890"),
            "not-a-timestamp",
            "todo",
            "2026-08-01T09:00:00Z",
            2,
        );
        cluster.witnessed_at = "not-a-timestamp".to_string();
        let card = build_cluster_card(&cluster, BoardColumn::OutdatedClaims, Some(1), test_now());
        assert_eq!(card.meta, "⌗abcdef12 · 2 items affected");
    }

    #[test]
    fn age_label_is_relative_and_absent_when_unparseable() {
        let now = test_now();
        assert_eq!(
            age_label("2026-08-11T00:00:00Z", now).as_deref(),
            Some("today")
        );
        assert_eq!(
            age_label("2026-08-10T00:00:00Z", now).as_deref(),
            Some("1d ago")
        );
        assert_eq!(
            age_label("2026-08-01T00:00:00Z", now).as_deref(),
            Some("10d ago")
        );
        assert_eq!(
            age_label("2026-01-01 00:00:00", now).as_deref(),
            Some("7mo ago")
        );
        assert_eq!(age_label("nonsense", now), None);
        assert_eq!(age_label("", now), None);
    }

    // --- detail --------------------------------------------------------------

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
        let detail = build_detail(&item, &DetailContext::default(), "tok".into());
        assert_eq!(detail.receipt, None);
        assert!(detail
            .evidence_lines
            .iter()
            .any(|line| line.contains("no receipt")));
    }

    #[test]
    fn detail_wording_comes_from_the_certified_projection() {
        let item = sample_item("id00", "proj", "finish the gate");
        let detail = build_detail(&item, &DetailContext::default(), "tok".into());
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
        let detail = build_detail(&item, &DetailContext::default(), "tok".into());
        assert!(detail
            .why_surfaced
            .contains("not about the item's own symbols"));
        assert_eq!(detail.grade_slug, "session-grade");
    }

    #[test]
    fn find_item_matches_only_a_stored_id() {
        let all: Vec<DreamItem> = (0..3)
            .map(|i| sample_item(&format!("id{i:02}"), "proj", &format!("item {i}")))
            .collect();
        assert!(find_item(&all, "id01").is_some());
        assert!(find_item(&all, "id99").is_none());
        assert!(find_item(&all, "../../etc/passwd").is_none());
    }

    #[test]
    fn static_feed_round_trips_through_the_trait() {
        let items: Vec<DreamItem> = (0..2)
            .map(|i| sample_item(&format!("id{i:02}"), "proj", &format!("item {i}")))
            .collect();
        let feed = StaticDreamFeed::new(items).with_last_pass("deadbeefcafe", "2026-08-09");
        let state = JournalState::new(Arc::new(feed));
        let loaded = state.feed().load().expect("static feed load");
        assert_eq!(loaded.len(), 2);
        assert_eq!(
            state.feed().last_pass(),
            Some(("deadbeefcafe".to_string(), "2026-08-09".to_string()))
        );
    }

    // --- csrf ----------------------------------------------------------------

    #[test]
    fn csrf_tokens_are_bound_to_the_item_and_to_the_process_key() {
        let key = CsrfKey::random();
        let a = key.token("id00");
        let b = key.token("id01");
        assert_ne!(a, b, "a token must not be reusable across items");
        assert!(key.verify("id00", &a));
        assert!(!key.verify("id01", &a), "a token must not cross items");
        assert!(!key.verify("id00", ""));
        assert!(!key.verify("id00", &a[..a.len() - 1]));

        let other = CsrfKey::random();
        assert_ne!(other.token("id00"), a, "keys must differ per process");
        assert!(!other.verify("id00", &a));
    }

    #[test]
    fn the_csrf_key_is_never_printed() {
        let key = CsrfKey::random();
        assert_eq!(format!("{key:?}"), "CsrfKey(<redacted>)");
    }

    // --- write path ----------------------------------------------------------

    /// The journal writes through the same ledger `csr_resolve` writes to, so
    /// its statuses must be inside that tool's accepted set. If `csr_resolve`
    /// ever narrows the set, this fails instead of the journal silently
    /// writing a status the rest of the system does not understand.
    #[tokio::test]
    async fn journal_statuses_are_exactly_what_csr_resolve_accepts() {
        let storage = Arc::new(Storage::open_memory().expect("memory storage"));
        for action in [JournalAction::Resolve, JournalAction::Dismiss] {
            crate::mcp::tools::resolve_chunks(
                &storage,
                vec!["chunk-1".to_string()],
                action.status().to_string(),
                "parity probe".to_string(),
                None,
            )
            .await
            .unwrap_or_else(|e| panic!("csr_resolve rejects {}: {e}", action.status()));
        }
    }

    #[test]
    fn dismiss_is_recorded_as_still_open_never_as_resolved() {
        assert_eq!(JournalAction::Resolve.status(), "resolved");
        assert_eq!(JournalAction::Dismiss.status(), "still_open");
        let item = sample_item("id00", "proj", "finish the gate");
        let evidence = JournalAction::Dismiss.evidence_for(&item);
        assert!(evidence.contains("a dismissal is not a resolution"));
        assert!(evidence.contains(JOURNAL_ORIGIN));
    }

    #[test]
    fn a_read_only_feed_refuses_the_write_instead_of_pretending() {
        let item = sample_item("id00", "proj", "finish the gate");
        let feed = StaticDreamFeed::new(vec![item.clone()]);
        assert_eq!(
            feed.record_verdict(&item, JournalAction::Resolve),
            Err(ResolveError::Unsupported)
        );
        assert!(feed.writes().is_empty());
    }

    /// The production write path, end to end, against a real database: the
    /// verdict lands in `resolution_ledger` tagged `journal_ui`, and an audit
    /// row records the same write.
    #[test]
    fn the_storage_feed_writes_a_journal_ui_verdict_and_an_audit_row() {
        let storage = Arc::new(Storage::open_memory().expect("memory storage"));
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO chunks
                        (id, conversation_id, project_name, timestamp, content, message_count)
                     VALUES ('chunk-1', 'sess-1', 'proj', '2026-08-01T00:00:00Z', 'body', 1)",
                    [],
                )?;
                Ok(())
            })
            .expect("seed chunk");

        let feed = StorageDreamFeed::new(storage.clone());
        let item = sample_item("id00", "proj", "finish the gate");
        let receipt = feed
            .record_verdict(&item, JournalAction::Dismiss)
            .expect("write");
        assert_eq!(receipt.status, "still_open");
        assert_eq!(receipt.origin, JOURNAL_ORIGIN);
        assert_eq!(receipt.chunks, 1);

        let (status, source, evidence): (String, String, String) = storage
            .with_connection(|conn| {
                Ok(conn.query_row(
                    "SELECT status, source, evidence FROM resolution_ledger
                     WHERE chunk_id = 'chunk-1' ORDER BY id DESC LIMIT 1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?)
            })
            .expect("ledger row");
        assert_eq!(status, "still_open");
        assert_eq!(source, JOURNAL_ORIGIN);
        assert!(evidence.contains("not actionable"));

        let (action, origin, chunks): (String, String, i64) = storage
            .with_connection(|conn| {
                Ok(conn.query_row(
                    "SELECT action, origin, chunk_count FROM journal_audit ORDER BY id DESC LIMIT 1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?)
            })
            .expect("audit row");
        assert_eq!(action, "dismiss");
        assert_eq!(origin, JOURNAL_ORIGIN);
        assert_eq!(chunks, 1);
    }

    #[test]
    fn a_session_with_no_chunks_reports_it_and_writes_nothing() {
        let storage = Arc::new(Storage::open_memory().expect("memory storage"));
        let feed = StorageDreamFeed::new(storage.clone());
        let item = sample_item("id00", "proj", "finish the gate");
        let error = feed
            .record_verdict(&item, JournalAction::Resolve)
            .expect_err("no chunks means no write");
        assert_eq!(
            error,
            ResolveError::NoChunks {
                session: "sess-1".to_string()
            }
        );
        assert!(error.to_string().contains("Nothing was written."));

        let count: i64 = storage
            .with_connection(|conn| {
                Ok(
                    conn.query_row("SELECT COUNT(*) FROM resolution_ledger", [], |row| {
                        row.get(0)
                    })?,
                )
            })
            .expect("count");
        assert_eq!(count, 0);
    }

    /// A failed detail read must surface as an error, not as an empty
    /// context that the template would render as "nothing on record"
    /// (codex X4 finding 9).
    #[test]
    fn a_failing_detail_read_is_an_error_not_an_empty_context() {
        let item = sample_item("id00", "proj", "finish the gate");
        let feed = StaticDreamFeed::new(vec![item.clone()]).with_detail_error("database is locked");
        let error = feed.detail_context(&item).expect_err("must propagate");
        assert!(error.to_string().contains("database is locked"));
    }

    #[test]
    fn a_successful_empty_detail_read_yields_an_empty_context() {
        let item = sample_item("id00", "proj", "finish the gate");
        let feed = StaticDreamFeed::new(vec![item.clone()]);
        let context = feed.detail_context(&item).expect("successful read");
        assert!(context.brief.empty);
        assert!(context.plan.is_none());
        assert!(context.churn.is_empty());
    }
}
