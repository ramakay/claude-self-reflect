//! Journal v4 Phase 3 — the before/after AST engine.
//!
//! Pure compute. No routes, no templates, no rendering, no SQL. Given a
//! repo-relative file path and two commit OIDs — "before" is the witness
//! ledger's `at_oid`, "after" is the verdict's `receipt_oid` (or the pass's
//! `observed_head_oid`) — this reads both blobs out of git via the
//! `codewitness` crate's already-vendored `gix` (features `revision`,
//! `blob-diff`, `sha1`), re-parses each side with `ast-grep`, and diffs the
//! two symbol sets into **intact / changed / removed / added**.
//!
//! The output is shaped for locked decision 9's *side-by-side trees*: two
//! source-ordered vectors ([`AstDiff::before`], [`AstDiff::after`]) plus
//! [`AstDiff::connectors`], one per symbol that survives both sides, so a
//! renderer can draw the join lines without re-deriving the pairing.
//!
//! # Honesty contract (binding)
//!
//! * **An empty tree is never a result.** Every failure — unreadable blob,
//!   unknown or malformed OID, file absent from *both* sides, unsupported
//!   language, non-UTF-8 blob, oversized blob, a parser panic, or a file
//!   from which neither side yielded a single named symbol — returns
//!   [`AstDiffOutcome::Abstained`] carrying the reason, and
//!   [`Abstention::sentence`] renders it. There is no code path that
//!   produces `Diffed` with two empty sides.
//! * **A file absent from exactly one side is not an abstention** — it is a
//!   real, witnessed fact (the file was created, or deleted). It is reported
//!   as [`AstDiff::before_present`] / [`AstDiff::after_present`] `= false`
//!   with the surviving side's symbols marked `Added` / `Removed`. A
//!   renderer must consult those flags before saying "no symbols".
//! * **Renames are never claimed.** A symbol whose name changed appears as
//!   one `Removed` and one `Added`. The engine has no evidence that the two
//!   are the same symbol, so it does not say they are — even when the body
//!   hashes match.
//! * **Churn is measured or absent, never estimated.** See [`ChurnTint`].
//! * **Truncation is disclosed.** [`AstDiff::before_total`] /
//!   [`AstDiff::after_total`] are the *measured* symbol counts before the
//!   per-side cap is applied, so a renderer can never present a truncated
//!   tree as complete.
//! * **The size guard bounds allocation, not just parsing.** A blob's size
//!   is taken from its git object *header* ([`inspect_blob`]) before any
//!   object data is decompressed, so a small compressed blob that inflates
//!   to hundreds of megabytes is refused at [`MAX_BLOB_BYTES`] without ever
//!   being materialised. See [`BlobPlan`].
//!
//! # Reuse
//!
//! Blob reads go through [`codewitness::Auditor::file_content_at`] rather
//! than a second git crate. Language detection, node kinds, name extraction
//! and body hashing come from `crate::extraction::{ast_analysis, anchors}`,
//! so this module and the anchor/codegraph pipelines agree on what a symbol
//! is and on when a body counts as changed (whitespace-normalized, so
//! rustfmt/prettier churn reads `Intact`).

use std::collections::{HashMap, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use ast_grep_core::matcher::KindMatcher;
use ast_grep_core::tree_sitter::LanguageExt;
use ast_grep_language::SupportLang;
use serde::{Serialize, Serializer};

use crate::extraction::anchors::hash_normalized;
use crate::extraction::ast_analysis::{
    extract_name_from_def, func_kinds, lang_from_path_str, type_kinds,
};
use crate::storage::dream_items::{last_two_segments, ChurnTile};

/// Largest blob either side may be before the engine abstains. Matches
/// `extraction::anchors::MAX_ANCHOR_FILE_BYTES` so a file that is too big to
/// anchor is also too big to diff.
///
/// Enforced against the blob's *declared* size, read from the git object
/// header before any content is decompressed — see [`inspect_blob`].
pub const MAX_BLOB_BYTES: usize = 512 * 1024;

/// Hard cap on rendered symbols per side. Exceeding it does not silently
/// drop evidence: the measured pre-truncation total is reported alongside.
pub const MAX_SYMBOLS_PER_SIDE: usize = 400;

/// Default entry cap for [`AstDiffCache`].
pub const DEFAULT_CACHE_ENTRIES: usize = 64;
/// Default total-byte cap for [`AstDiffCache`].
pub const DEFAULT_CACHE_BYTES: usize = 4 * 1024 * 1024;

/// Container node kinds whose name qualifies a member symbol
/// (`impl TokenCache { fn new }` → `TokenCache::new`).
///
/// Same list as `extraction::anchors::qualify_name`'s, which is a private
/// `fn` taking a `NodeMatch` and cannot be called from here. Keeping the two
/// in step matters: an unqualified `new` would collide across two `impl`
/// blocks in one file and pair the wrong nodes. Folding both onto one shared
/// helper is a follow-up that must touch `anchors.rs`.
const CONTAINER_KINDS: &[&str] = &["impl_item", "class_definition", "class_declaration"];

// --- churn -------------------------------------------------------------------

/// A touch count that was actually measured.
///
/// The field is private to this module, so **no code outside
/// `journal::astdiff` can construct one** — not even elsewhere in this
/// crate. The only producer is [`ChurnTint::from_churn`], which copies a
/// count out of a [`ChurnTile`] row, and `load_churn` only emits tiles for
/// files it actually counted. That is the type-level half of locked
/// decision 10's "measured or absent, never estimated".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MeasuredTouches(u32);

/// Churn tint for a node or a file: either a measured touch count, or
/// nothing at all.
///
/// There is deliberately no `From<u32>`, no `Default` carrying a zero, and
/// no public constructor taking a number. [`ChurnTint::touches`] returns
/// `Option<u32>`, and the [`Serialize`] impl emits the number or `null` —
/// so a template cannot print a count that was never measured, and
/// `Unmeasured` cannot be mistaken for a measured zero.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ChurnTint {
    /// No measurement exists for this file. Renders as nothing. It is the
    /// `Default` on purpose: a value that was never populated must read as
    /// "not measured", never as a count of zero.
    #[default]
    Unmeasured,
    /// A count taken from a measured churn row.
    Measured(MeasuredTouches),
}

impl ChurnTint {
    /// Look `file` up in a set of **measured** churn tiles.
    ///
    /// Matching uses [`last_two_segments`], the same idiom
    /// `dream_items::load_anchor_tree` uses to join verdict rows to files,
    /// because churn tiles record whatever path the episode wrote
    /// (frequently absolute) while a diff path is repo-relative. No match →
    /// [`ChurnTint::Unmeasured`]; the absence of a row never becomes a zero.
    pub fn from_churn(file: &str, churn: &[ChurnTile]) -> Self {
        let key = last_two_segments(file);
        churn
            .iter()
            .find(|tile| last_two_segments(&tile.file) == key)
            .map(|tile| ChurnTint::Measured(MeasuredTouches(tile.touches)))
            .unwrap_or(ChurnTint::Unmeasured)
    }

    /// The measured count, or `None`. The only way to read a number out.
    pub fn touches(self) -> Option<u32> {
        match self {
            ChurnTint::Unmeasured => None,
            ChurnTint::Measured(MeasuredTouches(n)) => Some(n),
        }
    }

    /// Whether a measurement exists. `Measured(0)` is `true`; `Unmeasured`
    /// is `false`. They are different facts and never collapse.
    pub fn is_measured(self) -> bool {
        matches!(self, ChurnTint::Measured(_))
    }
}

impl Serialize for ChurnTint {
    /// Serializes to the number or to `null`. A template writing the value
    /// directly gets nothing for an unmeasured node, and `{% if churn %}`
    /// is false — there is no representation of "unmeasured" that looks
    /// like a count.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.touches().serialize(serializer)
    }
}

// --- abstention --------------------------------------------------------------

/// Which OID a failure belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Before,
    After,
}

impl Side {
    pub fn label(self) -> &'static str {
        match self {
            Side::Before => "before",
            Side::After => "after",
        }
    }
}

impl std::fmt::Display for Side {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Why the engine declined to produce a diff. Every variant carries enough
/// to render a specific sentence — none of them degrade to "unknown error".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum Abstention {
    /// `repo_root` is not an openable git repository.
    RepositoryUnavailable { repo_root: String, detail: String },
    /// The path was empty, absolute, or contained a `..` component. Rejected
    /// lexically, before any git or filesystem access.
    PathRejected { path: String, detail: String },
    /// The OID string is not 4–64 hexadecimal characters. Rejecting
    /// non-hex input also means no rev-spec syntax (`HEAD`, `@{…}`, `..`)
    /// can reach git from a request parameter.
    MalformedOid { side: Side, oid: String },
    /// Well-formed hex, but the repository has no such commit.
    UnknownOid {
        side: Side,
        oid: String,
        detail: String,
    },
    /// The extension maps to none of the six supported languages.
    UnsupportedLanguage { path: String, extension: String },
    /// The blob exists but exceeds [`MAX_BLOB_BYTES`].
    FileTooLarge {
        side: Side,
        path: String,
        bytes: usize,
        limit: usize,
    },
    /// The tree entry exists but could not be read as a file blob (a
    /// directory, submodule pointer, or symlink at that path), or the
    /// object database refused the read.
    UnreadableBlob {
        side: Side,
        path: String,
        detail: String,
    },
    /// The blob is not valid UTF-8 — a binary file, or a text file with a
    /// broken encoding. There is nothing to parse.
    NotUtf8 { side: Side, path: String },
    /// The tree-sitter parse panicked. Caught, never propagated.
    ParsePanicked { side: Side, path: String },
    /// The file exists at neither commit.
    AbsentFromBothSides {
        path: String,
        before_oid: String,
        after_oid: String,
    },
    /// Both sides parsed, but neither produced a single named symbol (a
    /// file of only comments, imports or `mod` lines; or source so
    /// malformed that tree-sitter recovered nothing). Reported as an
    /// abstention rather than an empty diff, which would render as
    /// "nothing changed".
    NoSymbolsExtracted {
        path: String,
        before_oid: String,
        after_oid: String,
    },
}

impl Abstention {
    /// The sentence the AST slot renders instead of a tree. Always leads
    /// with the abstention so it cannot be mistaken for a finding.
    pub fn sentence(&self) -> String {
        match self {
            Abstention::RepositoryUnavailable { repo_root, detail } => format!(
                "AST comparison abstained: {repo_root} could not be opened as a git repository ({detail})."
            ),
            Abstention::PathRejected { path, detail } => {
                format!("AST comparison abstained: the path {path} was rejected ({detail}).")
            }
            Abstention::MalformedOid { side, oid } => format!(
                "AST comparison abstained: the {side} commit id {oid} is not a hexadecimal object id."
            ),
            Abstention::UnknownOid { side, oid, detail } => format!(
                "AST comparison abstained: this repository has no commit {oid} for the {side} side ({detail})."
            ),
            Abstention::UnsupportedLanguage { path, extension } => format!(
                "AST comparison abstained: no parser for {path} (extension {extension} is outside the six supported languages)."
            ),
            Abstention::FileTooLarge {
                side,
                path,
                bytes,
                limit,
            } => format!(
                "AST comparison abstained: {path} is {bytes} bytes at the {side} commit, over the {limit}-byte parse limit."
            ),
            Abstention::UnreadableBlob { side, path, detail } => format!(
                "AST comparison abstained: {path} could not be read at the {side} commit ({detail})."
            ),
            Abstention::NotUtf8 { side, path } => format!(
                "AST comparison abstained: {path} is not valid UTF-8 at the {side} commit, so it cannot be parsed."
            ),
            Abstention::ParsePanicked { side, path } => format!(
                "AST comparison abstained: the parser failed on {path} at the {side} commit."
            ),
            Abstention::AbsentFromBothSides {
                path,
                before_oid,
                after_oid,
            } => format!(
                "AST comparison abstained: {path} is present at neither {before_oid} nor {after_oid}."
            ),
            Abstention::NoSymbolsExtracted {
                path,
                before_oid,
                after_oid,
            } => format!(
                "AST comparison abstained: no named symbol was extracted from {path} at {before_oid} or {after_oid}."
            ),
        }
    }

    /// Whether this outcome may be cached.
    ///
    /// Git objects are immutable, so anything derived purely from a path or
    /// from blob content at two fixed OIDs is stable forever. Availability
    /// failures are not: a later fetch can supply a missing commit, and an
    /// odb error can be transient. Those are recomputed every time rather
    /// than pinned into the cache as a permanent verdict.
    pub fn is_stable(&self) -> bool {
        match self {
            Abstention::RepositoryUnavailable { .. }
            | Abstention::UnknownOid { .. }
            | Abstention::UnreadableBlob { .. } => false,
            Abstention::PathRejected { .. }
            | Abstention::MalformedOid { .. }
            | Abstention::UnsupportedLanguage { .. }
            | Abstention::FileTooLarge { .. }
            | Abstention::NotUtf8 { .. }
            | Abstention::ParsePanicked { .. }
            | Abstention::AbsentFromBothSides { .. }
            | Abstention::NoSymbolsExtracted { .. } => true,
        }
    }
}

// --- diff shape --------------------------------------------------------------

/// What happened to one symbol. `Removed` only ever appears on the before
/// side and `Added` only on the after side; `Intact` and `Changed` appear on
/// both, once per side of a connector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolStatus {
    Intact,
    Changed,
    Removed,
    Added,
}

impl SymbolStatus {
    pub fn label(self) -> &'static str {
        match self {
            SymbolStatus::Intact => "intact",
            SymbolStatus::Changed => "changed",
            SymbolStatus::Removed => "removed",
            SymbolStatus::Added => "added",
        }
    }
}

/// One symbol on one side of the comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SymbolNode {
    /// Container-qualified name (`TokenCache::new`), the pairing key
    /// together with [`SymbolNode::kind`].
    pub name: String,
    /// The tree-sitter node kind (`function_item`, `class_declaration`, …).
    pub kind: String,
    /// The enclosing `impl`/`class` name, when there is one. A render hint
    /// for tree indentation; the flat vector stays in source order.
    pub container: Option<String>,
    /// `0` for a top-level symbol, `1` for a member of a container.
    pub depth: usize,
    /// 1-based start line at this side's commit.
    pub line: usize,
    /// Position of this node within its own side, source order.
    pub order: usize,
    /// Whitespace-normalized body hash (`extraction::anchors`), so
    /// reformatting reads `Intact`.
    pub body_hash: String,
    pub status: SymbolStatus,
    /// Index into the *other* side's vector when this symbol survives both.
    pub paired_with: Option<usize>,
    /// The measured churn of the file this node lives in.
    ///
    /// CSR measures touches per **file**, never per symbol, so every node
    /// on a side carries the same value. It is the file's context, not a
    /// per-symbol measurement, and must be captioned as such (locked
    /// decision 10: churn is context, never importance).
    pub churn: ChurnTint,
}

/// One join line between the two trees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Connector {
    /// Index into [`AstDiff::before`].
    pub before: usize,
    /// Index into [`AstDiff::after`].
    pub after: usize,
    /// `true` when the paired bodies differ.
    pub changed: bool,
}

/// Counts of rows actually produced. Nothing here is inferred from the
/// absence of a row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct DiffCounts {
    pub intact: usize,
    pub changed: usize,
    pub removed: usize,
    pub added: usize,
}

/// The side-by-side comparison of one file at two commits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AstDiff {
    pub path: String,
    /// Language label, from the extension.
    pub language: String,
    pub before_oid: String,
    pub after_oid: String,
    /// Whether the file existed at `before_oid`. `false` with an empty
    /// `before` vector means "created", not "nothing to show".
    pub before_present: bool,
    /// Whether the file existed at `after_oid`. `false` with an empty
    /// `after` vector means "deleted", not "nothing to show".
    pub after_present: bool,
    /// Before-side symbols, source order.
    pub before: Vec<SymbolNode>,
    /// After-side symbols, source order.
    pub after: Vec<SymbolNode>,
    /// Measured before-side symbol count prior to [`MAX_SYMBOLS_PER_SIDE`].
    pub before_total: usize,
    /// Measured after-side symbol count prior to [`MAX_SYMBOLS_PER_SIDE`].
    pub after_total: usize,
    /// Pairs, in before-side order.
    pub connectors: Vec<Connector>,
    pub counts: DiffCounts,
    /// The file's measured churn, or absent. Same value every node carries.
    pub churn: ChurnTint,
}

impl AstDiff {
    /// Whether either side was truncated at [`MAX_SYMBOLS_PER_SIDE`].
    pub fn truncated(&self) -> bool {
        self.before_total > self.before.len() || self.after_total > self.after.len()
    }
}

/// The engine's only return type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AstDiffOutcome {
    Diffed(AstDiff),
    Abstained(Abstention),
}

impl AstDiffOutcome {
    pub fn diff(&self) -> Option<&AstDiff> {
        match self {
            AstDiffOutcome::Diffed(diff) => Some(diff),
            AstDiffOutcome::Abstained(_) => None,
        }
    }

    pub fn abstention(&self) -> Option<&Abstention> {
        match self {
            AstDiffOutcome::Abstained(reason) => Some(reason),
            AstDiffOutcome::Diffed(_) => None,
        }
    }

    /// Attach measured churn. Kept off [`compute`] so the cache key stays
    /// exactly `(path, before_oid, after_oid)` — churn changes as episodes
    /// accumulate and must not be pinned into a structural cache entry.
    pub fn with_churn(mut self, churn: &[ChurnTile]) -> Self {
        if let AstDiffOutcome::Diffed(diff) = &mut self {
            let tint = ChurnTint::from_churn(&diff.path, churn);
            diff.churn = tint;
            for node in diff.before.iter_mut().chain(diff.after.iter_mut()) {
                node.churn = tint;
            }
        }
        self
    }
}

/// What to compare.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AstDiffRequest {
    /// The repository root (the directory containing `.git`).
    pub repo_root: PathBuf,
    /// Repo-relative path, forward-slash separated.
    pub path: String,
    /// Witness ledger `at_oid`.
    pub before_oid: String,
    /// Verdict `receipt_oid`, or the pass's `observed_head_oid`.
    pub after_oid: String,
}

// --- compute -----------------------------------------------------------------

/// One side's blob, or the fact that the file was not there.
#[derive(Debug)]
enum SideRead {
    Present(String),
    Absent,
}

/// A symbol as extracted, before pairing.
struct RawSymbol {
    name: String,
    kind: String,
    container: Option<String>,
    line: usize,
    byte_start: usize,
    body_hash: String,
}

/// Compare `path` at two commits.
///
/// Never panics and never returns an empty `Diffed`. Checks run cheapest
/// first, so the reported reason is the first thing that was actually
/// wrong: path shape → OID shape → language → repository → OID resolution →
/// blob reads → parse → extraction.
///
/// The result carries no churn; call [`AstDiffOutcome::with_churn`] to
/// attach measured tiles.
pub fn compute(request: &AstDiffRequest) -> AstDiffOutcome {
    match compute_inner(request) {
        Ok(diff) => AstDiffOutcome::Diffed(diff),
        Err(reason) => AstDiffOutcome::Abstained(reason),
    }
}

fn compute_inner(request: &AstDiffRequest) -> Result<AstDiff, Abstention> {
    let relative = validate_path(&request.path)?;
    validate_oid(&request.before_oid, Side::Before)?;
    validate_oid(&request.after_oid, Side::After)?;

    let lang =
        lang_from_path_str(&request.path).ok_or_else(|| Abstention::UnsupportedLanguage {
            path: request.path.clone(),
            extension: extension_of(&request.path),
        })?;

    let auditor = codewitness::Auditor::open(request.repo_root.clone()).map_err(|error| {
        Abstention::RepositoryUnavailable {
            repo_root: request.repo_root.to_string_lossy().into_owned(),
            detail: error.to_string(),
        }
    })?;

    let before_commit = resolve_oid(&auditor, &request.before_oid, Side::Before)?;
    let after_commit = resolve_oid(&auditor, &request.after_oid, Side::After)?;

    let before_read = read_side(
        &auditor,
        &relative,
        &request.path,
        before_commit,
        Side::Before,
    )?;
    let after_read = read_side(
        &auditor,
        &relative,
        &request.path,
        after_commit,
        Side::After,
    )?;

    let (before_present, before_source) = match &before_read {
        SideRead::Present(text) => (true, Some(text.as_str())),
        SideRead::Absent => (false, None),
    };
    let (after_present, after_source) = match &after_read {
        SideRead::Present(text) => (true, Some(text.as_str())),
        SideRead::Absent => (false, None),
    };

    if !before_present && !after_present {
        return Err(Abstention::AbsentFromBothSides {
            path: request.path.clone(),
            before_oid: request.before_oid.clone(),
            after_oid: request.after_oid.clone(),
        });
    }

    let before_raw = match before_source {
        Some(text) => extract_symbols(text, lang).ok_or(Abstention::ParsePanicked {
            side: Side::Before,
            path: request.path.clone(),
        })?,
        None => Vec::new(),
    };
    let after_raw = match after_source {
        Some(text) => extract_symbols(text, lang).ok_or(Abstention::ParsePanicked {
            side: Side::After,
            path: request.path.clone(),
        })?,
        None => Vec::new(),
    };

    if before_raw.is_empty() && after_raw.is_empty() {
        return Err(Abstention::NoSymbolsExtracted {
            path: request.path.clone(),
            before_oid: request.before_oid.clone(),
            after_oid: request.after_oid.clone(),
        });
    }

    let before_total = before_raw.len();
    let after_total = after_raw.len();
    let before_raw: Vec<RawSymbol> = before_raw.into_iter().take(MAX_SYMBOLS_PER_SIDE).collect();
    let after_raw: Vec<RawSymbol> = after_raw.into_iter().take(MAX_SYMBOLS_PER_SIDE).collect();

    let (pair_of_before, pair_of_after) = pair_sides(&before_raw, &after_raw);

    let mut connectors = Vec::new();
    let mut counts = DiffCounts::default();
    let mut before_nodes = Vec::with_capacity(before_raw.len());
    for (index, raw) in before_raw.iter().enumerate() {
        let (status, paired) = match pair_of_before[index] {
            None => {
                counts.removed += 1;
                (SymbolStatus::Removed, None)
            }
            Some(other) => {
                let changed = raw.body_hash != after_raw[other].body_hash;
                if changed {
                    counts.changed += 1;
                } else {
                    counts.intact += 1;
                }
                connectors.push(Connector {
                    before: index,
                    after: other,
                    changed,
                });
                (
                    if changed {
                        SymbolStatus::Changed
                    } else {
                        SymbolStatus::Intact
                    },
                    Some(other),
                )
            }
        };
        before_nodes.push(node_from(raw, index, status, paired));
    }

    let mut after_nodes = Vec::with_capacity(after_raw.len());
    for (index, raw) in after_raw.iter().enumerate() {
        let (status, paired) = match pair_of_after[index] {
            None => {
                counts.added += 1;
                (SymbolStatus::Added, None)
            }
            Some(other) => {
                let changed = raw.body_hash != before_raw[other].body_hash;
                (
                    if changed {
                        SymbolStatus::Changed
                    } else {
                        SymbolStatus::Intact
                    },
                    Some(other),
                )
            }
        };
        after_nodes.push(node_from(raw, index, status, paired));
    }

    // The honesty contract restated at the one place an `AstDiff` is built.
    // Two empty sides are already unreachable — the `NoSymbolsExtracted`
    // check above returns first, and the per-side cap is >= 1 so truncation
    // cannot empty a non-empty side — but this is the invariant the whole
    // module promises, so it is enforced where the value is constructed
    // rather than inferred from the control flow above it.
    if before_nodes.is_empty() && after_nodes.is_empty() {
        return Err(Abstention::NoSymbolsExtracted {
            path: request.path.clone(),
            before_oid: request.before_oid.clone(),
            after_oid: request.after_oid.clone(),
        });
    }

    Ok(AstDiff {
        path: request.path.clone(),
        language: format!("{lang:?}"),
        before_oid: request.before_oid.clone(),
        after_oid: request.after_oid.clone(),
        before_present,
        after_present,
        before: before_nodes,
        after: after_nodes,
        before_total,
        after_total,
        connectors,
        counts,
        churn: ChurnTint::Unmeasured,
    })
}

fn node_from(
    raw: &RawSymbol,
    order: usize,
    status: SymbolStatus,
    paired_with: Option<usize>,
) -> SymbolNode {
    SymbolNode {
        name: raw.name.clone(),
        kind: raw.kind.clone(),
        depth: usize::from(raw.container.is_some()),
        container: raw.container.clone(),
        line: raw.line,
        order,
        body_hash: raw.body_hash.clone(),
        status,
        paired_with,
        churn: ChurnTint::Unmeasured,
    }
}

/// Greedy first-come pairing on `(kind, name)`, walking the before side in
/// source order and consuming after-side candidates in source order. Two
/// same-named siblings therefore pair positionally and deterministically.
/// A name that exists on only one side is left unpaired — the engine does
/// not guess at renames.
#[allow(clippy::type_complexity)]
fn pair_sides(
    before: &[RawSymbol],
    after: &[RawSymbol],
) -> (Vec<Option<usize>>, Vec<Option<usize>>) {
    let mut buckets: HashMap<(&str, &str), VecDeque<usize>> = HashMap::new();
    for (index, raw) in after.iter().enumerate() {
        buckets
            .entry((raw.kind.as_str(), raw.name.as_str()))
            .or_default()
            .push_back(index);
    }

    let mut pair_of_before = vec![None; before.len()];
    let mut pair_of_after = vec![None; after.len()];
    for (index, raw) in before.iter().enumerate() {
        if let Some(queue) = buckets.get_mut(&(raw.kind.as_str(), raw.name.as_str())) {
            if let Some(other) = queue.pop_front() {
                pair_of_before[index] = Some(other);
                pair_of_after[other] = Some(index);
            }
        }
    }
    (pair_of_before, pair_of_after)
}

/// Lexical path guard. Runs before any git or filesystem access, so a
/// hostile request parameter cannot escape the repository or name an
/// absolute path.
fn validate_path(path: &str) -> Result<PathBuf, Abstention> {
    let reject = |detail: &str| Abstention::PathRejected {
        path: path.to_string(),
        detail: detail.to_string(),
    };
    if path.trim().is_empty() {
        return Err(reject("empty"));
    }
    if path.contains('\0') {
        return Err(reject("contains a NUL byte"));
    }
    let candidate = PathBuf::from(path);
    if candidate.is_absolute() {
        return Err(reject("absolute paths are not repo-relative"));
    }
    if candidate
        .components()
        .any(|component| component == Component::ParentDir)
    {
        return Err(reject("contains a `..` component"));
    }
    Ok(candidate)
}

/// An OID parameter must be 4–64 hex characters. This is not only a
/// well-formedness check: rejecting everything non-hex means no rev-spec
/// syntax (`HEAD`, `main@{1}`, `a..b`, `:/message`) can reach git through a
/// request parameter, even though `resolve_commit` would otherwise accept it.
fn validate_oid(oid: &str, side: Side) -> Result<(), Abstention> {
    let trimmed = oid.trim();
    let malformed = || Abstention::MalformedOid {
        side,
        oid: oid.to_string(),
    };
    if !(4..=64).contains(&trimmed.len()) {
        return Err(malformed());
    }
    if !trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(malformed());
    }
    Ok(())
}

fn resolve_oid(
    auditor: &codewitness::Auditor,
    oid: &str,
    side: Side,
) -> Result<codewitness::ObjectId, Abstention> {
    auditor
        .resolve_commit(oid.trim())
        .map_err(|error| Abstention::UnknownOid {
            side,
            oid: oid.trim().to_string(),
            detail: error.to_string(),
        })
}

/// What the tree entry at a path declares, read from metadata alone.
///
/// Producing one of these decompresses **no object data**. It reads the
/// commit, its tree, one tree entry, and that entry's object *header* via
/// `gix`'s `find_header`, which resolves a packed object's delta chain far
/// enough to learn the final size but never inflates the payload.
///
/// That ordering is the whole point of the type. Checking a size after
/// [`codewitness::Auditor::file_content_at`] has already returned a
/// complete `Vec<u8>` bounds *parsing and caching* but not memory: a blob
/// that zlib-compresses to a few kilobytes and inflates to hundreds of
/// megabytes would be fully allocated on every request before the guard
/// ever ran. Deciding from the header means [`MAX_BLOB_BYTES`] caps the
/// allocation itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlobPlan {
    /// The commit's tree has no entry at that path.
    Absent,
    /// A regular or executable file blob declaring `bytes` bytes.
    Blob { bytes: u64 },
}

/// Read the tree entry at `relative` in `commit` and report what it
/// declares, without materialising the object.
///
/// Everything that is not a readable regular-file blob abstains here with a
/// reason: a tree, a submodule pointer, a symlink, a missing or undecodable
/// commit, an unreadable tree, or an object database that cannot produce a
/// header. A path simply not present in the tree is [`BlobPlan::Absent`] —
/// a witnessed fact (created/deleted), not a failure.
///
/// Symlinks are rejected on the tree entry's mode rather than on the object
/// kind. Git stores a symlink's *target path* as an ordinary blob, so
/// object-level checks accept one and hand back the target string as if it
/// were file content; the mode (`120000`) is the only place the distinction
/// survives.
fn inspect_blob(
    auditor: &codewitness::Auditor,
    relative: &Path,
    display_path: &str,
    commit: codewitness::ObjectId,
    side: Side,
) -> Result<BlobPlan, Abstention> {
    let unreadable = |detail: String| Abstention::UnreadableBlob {
        side,
        path: display_path.to_string(),
        detail,
    };

    let repo = auditor.repo();
    let commit_object = repo
        .find_commit(commit)
        .map_err(|error| unreadable(error.to_string()))?;
    let tree = commit_object
        .tree()
        .map_err(|error| unreadable(error.to_string()))?;
    let entry = match tree
        .lookup_entry_by_path(relative)
        .map_err(|error| unreadable(error.to_string()))?
    {
        Some(entry) => entry,
        None => return Ok(BlobPlan::Absent),
    };
    if !entry.mode().is_blob() {
        return Err(unreadable(
            "the path names a tree, submodule or symlink, not a file".to_string(),
        ));
    }
    let header = repo
        .find_header(entry.object_id())
        .map_err(|error| unreadable(error.to_string()))?;
    if !header.kind().is_blob() {
        return Err(unreadable(format!(
            "the object at that path is a {:?}, not a file blob",
            header.kind()
        )));
    }
    Ok(BlobPlan::Blob {
        bytes: header.size(),
    })
}

fn read_side(
    auditor: &codewitness::Auditor,
    relative: &Path,
    display_path: &str,
    commit: codewitness::ObjectId,
    side: Side,
) -> Result<SideRead, Abstention> {
    let too_large = |bytes: u64| Abstention::FileTooLarge {
        side,
        path: display_path.to_string(),
        // Saturating rather than wrapping: on a 32-bit target a blob that
        // declares more than `usize::MAX` bytes is still reported as over
        // the limit, never as a small number.
        bytes: usize::try_from(bytes).unwrap_or(usize::MAX),
        limit: MAX_BLOB_BYTES,
    };

    // Metadata first. Running the guard after the read would bound parsing
    // and caching but not allocation, which is the whole exposure.
    match inspect_blob(auditor, relative, display_path, commit, side)? {
        // The tree simply has no entry there — a real, reportable fact, not
        // a failure. Exactly one of the two sides may be Absent.
        BlobPlan::Absent => return Ok(SideRead::Absent),
        BlobPlan::Blob { bytes } if bytes > MAX_BLOB_BYTES as u64 => return Err(too_large(bytes)),
        BlobPlan::Blob { .. } => {}
    }

    let bytes = match auditor.file_content_at(relative, commit) {
        Ok(bytes) => bytes,
        // `inspect_blob` already resolved these, so reaching one here means
        // the object database changed underneath us (a concurrent `git gc`,
        // a pruned pack). Report it, never fall through to an empty side.
        Err(codewitness::Error::AnchorMissing { .. }) => return Ok(SideRead::Absent),
        Err(codewitness::Error::NotABlob { .. }) => {
            return Err(Abstention::UnreadableBlob {
                side,
                path: display_path.to_string(),
                detail: "the path names a tree, submodule or symlink, not a file".to_string(),
            })
        }
        Err(error) => {
            return Err(Abstention::UnreadableBlob {
                side,
                path: display_path.to_string(),
                detail: error.to_string(),
            })
        }
    };
    // Belt and braces: the header said it fit. If the materialised object
    // disagrees — a corrupt header, or the odb swapped between the two
    // reads — refuse rather than parse it.
    if bytes.len() > MAX_BLOB_BYTES {
        return Err(too_large(bytes.len() as u64));
    }
    match String::from_utf8(bytes) {
        Ok(text) => Ok(SideRead::Present(text)),
        Err(_) => Err(Abstention::NotUtf8 {
            side,
            path: display_path.to_string(),
        }),
    }
}

fn extension_of(path: &str) -> String {
    Path::new(path)
        .extension()
        .map(|ext| ext.to_string_lossy().into_owned())
        .unwrap_or_else(|| "none".to_string())
}

/// Extract every named function-like and type-like node, in source order.
///
/// `None` means the parser panicked (caught, matching
/// `extraction::anchors::capture_file_anchors`'s precedent) — the caller
/// turns that into [`Abstention::ParsePanicked`] rather than an empty side.
/// `Some(vec![])` means the parse succeeded and the file genuinely has no
/// named symbols; those two facts are never conflated.
fn extract_symbols(source: &str, lang: SupportLang) -> Option<Vec<RawSymbol>> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let grep = lang.ast_grep(source);
        let root = grep.root();
        let mut out: Vec<RawSymbol> = Vec::new();
        for kind in func_kinds(lang).iter().chain(type_kinds(lang).iter()) {
            let matcher = KindMatcher::new(kind, lang);
            for node in root.find_all(&matcher) {
                let Some(bare) = extract_name_from_def(&node, lang) else {
                    continue;
                };
                let container = container_of(&node);
                let name = match &container {
                    Some(outer) => format!("{outer}::{bare}"),
                    None => bare,
                };
                out.push(RawSymbol {
                    name,
                    kind: (*kind).to_string(),
                    container,
                    line: node.start_pos().line() + 1,
                    byte_start: node.range().start,
                    body_hash: hash_normalized(&node.text()),
                });
            }
        }
        // Source order, with a total tie-break so two nodes starting at the
        // same byte still order deterministically across runs.
        out.sort_by(|a, b| {
            a.byte_start
                .cmp(&b.byte_start)
                .then_with(|| a.kind.cmp(&b.kind))
                .then_with(|| a.name.cmp(&b.name))
        });
        out
    }))
    .ok()
}

fn container_of<D: ast_grep_core::Doc>(node: &ast_grep_core::NodeMatch<'_, D>) -> Option<String> {
    for ancestor in node.ancestors() {
        if CONTAINER_KINDS.contains(&ancestor.kind().as_ref()) {
            // tree-sitter field access: `impl_item` carries `type`, class
            // declarations carry `name`.
            if let Some(named) = ancestor.field("name").or_else(|| ancestor.field("type")) {
                return Some(named.text().to_string());
            }
        }
    }
    None
}

// --- cache -------------------------------------------------------------------

/// Cache key, exactly `(path, before_oid, after_oid)`. `repo_root` is part
/// of it too because the same relative path means different files in
/// different repositories.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    repo_root: PathBuf,
    path: String,
    before_oid: String,
    after_oid: String,
}

impl CacheKey {
    fn of(request: &AstDiffRequest) -> Self {
        Self {
            repo_root: request.repo_root.clone(),
            path: request.path.clone(),
            before_oid: request.before_oid.trim().to_string(),
            after_oid: request.after_oid.trim().to_string(),
        }
    }
}

struct CacheEntry {
    value: Arc<AstDiffOutcome>,
    bytes: usize,
    used: u64,
}

#[derive(Default)]
struct CacheInner {
    entries: HashMap<CacheKey, CacheEntry>,
    bytes: usize,
    tick: u64,
    hits: u64,
    misses: u64,
    evictions: u64,
    rejected_oversize: u64,
}

/// Observable cache state, for status output and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheStats {
    pub entries: usize,
    pub bytes: usize,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    /// Outcomes that were computed but never stored because a single entry
    /// exceeded the whole byte budget.
    pub rejected_oversize: u64,
}

/// Bounded LRU over [`compute`], capped on both entry count and total bytes.
///
/// Values are churn-free (see [`AstDiffOutcome::with_churn`]) so a cached
/// entry can never serve a stale touch count.
///
/// Concurrency: the mutex is held only for lookup and insert, never across
/// the git read and parse. Two threads racing the same cold key may both
/// compute; the second insert simply replaces the first. That costs
/// duplicated work, never a wrong answer.
pub struct AstDiffCache {
    inner: Mutex<CacheInner>,
    max_entries: usize,
    max_bytes: usize,
}

impl Default for AstDiffCache {
    fn default() -> Self {
        Self::with_limits(DEFAULT_CACHE_ENTRIES, DEFAULT_CACHE_BYTES)
    }
}

impl AstDiffCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// `max_entries` and `max_bytes` are both floored at 1 so a zero-capacity
    /// cache cannot spin evicting forever.
    pub fn with_limits(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            inner: Mutex::new(CacheInner::default()),
            max_entries: max_entries.max(1),
            max_bytes: max_bytes.max(1),
        }
    }

    /// Cached [`compute`]. Availability abstentions
    /// ([`Abstention::is_stable`] `== false`) are recomputed every call.
    pub fn get_or_compute(&self, request: &AstDiffRequest) -> Arc<AstDiffOutcome> {
        let key = CacheKey::of(request);
        if let Some(hit) = self.lookup(&key) {
            return hit;
        }
        let value = Arc::new(compute(request));
        let cacheable = match value.as_ref() {
            AstDiffOutcome::Diffed(_) => true,
            AstDiffOutcome::Abstained(reason) => reason.is_stable(),
        };
        if cacheable {
            self.insert(key, value.clone());
        }
        value
    }

    pub fn stats(&self) -> CacheStats {
        let inner = self.lock();
        CacheStats {
            entries: inner.entries.len(),
            bytes: inner.bytes,
            hits: inner.hits,
            misses: inner.misses,
            evictions: inner.evictions,
            rejected_oversize: inner.rejected_oversize,
        }
    }

    pub fn clear(&self) {
        let mut inner = self.lock();
        inner.entries.clear();
        inner.bytes = 0;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CacheInner> {
        // A panic inside a handler must not poison the whole journal.
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn lookup(&self, key: &CacheKey) -> Option<Arc<AstDiffOutcome>> {
        let mut inner = self.lock();
        inner.tick += 1;
        let tick = inner.tick;
        match inner.entries.get_mut(key) {
            Some(entry) => {
                entry.used = tick;
                let value = entry.value.clone();
                inner.hits += 1;
                Some(value)
            }
            None => {
                inner.misses += 1;
                None
            }
        }
    }

    fn insert(&self, key: CacheKey, value: Arc<AstDiffOutcome>) {
        let bytes = approx_bytes(value.as_ref());
        let mut inner = self.lock();
        if bytes > self.max_bytes {
            // Storing it would immediately evict everything else and still
            // breach the bound. Serve it, don't keep it.
            inner.rejected_oversize += 1;
            return;
        }
        inner.tick += 1;
        let tick = inner.tick;
        if let Some(old) = inner.entries.remove(&key) {
            inner.bytes -= old.bytes;
        }
        inner.bytes += bytes;
        inner.entries.insert(
            key,
            CacheEntry {
                value,
                bytes,
                used: tick,
            },
        );
        while inner.entries.len() > self.max_entries || inner.bytes > self.max_bytes {
            let Some(victim) = inner
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            if let Some(entry) = inner.entries.remove(&victim) {
                inner.bytes -= entry.bytes;
                inner.evictions += 1;
            }
        }
    }
}

/// Serialized size, used only to enforce the byte bound. `AstDiff` derives
/// `Serialize` for the renderer anyway, so this reuses that rather than
/// hand-maintaining a size formula that would silently drift when a field is
/// added. Serialization cost is trivial next to the git read and re-parse it
/// is protecting. A failure returns `usize::MAX`, which refuses caching
/// rather than under-counting the bound.
fn approx_bytes(outcome: &AstDiffOutcome) -> usize {
    serde_json::to_string(outcome)
        .map(|json| json.len())
        .unwrap_or(usize::MAX)
}

// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    // --- fixture repositories -------------------------------------------
    //
    // Real `git init` + real commits + the real OIDs they produce, per the
    // `tests/dream_integration.rs` precedent. Nothing here fakes an OID.

    /// `git -C <repo>` with the caller's own `GIT_*` environment stripped —
    /// this suite may run under a git hook that exports `GIT_DIR`, which
    /// would redirect these commands at the real repository.
    fn git_in(repo: &Path) -> Command {
        let mut cmd = Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                cmd.env_remove(&key);
            }
        }
        cmd.arg("-C").arg(repo);
        cmd
    }

    struct Fixture {
        dir: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let repo = dir.path();
            assert!(
                git_in(repo)
                    .args(["init", "-q", "-b", "main"])
                    .status()
                    .expect("git must be available to run the astdiff fixtures")
                    .success(),
                "git init failed"
            );
            for args in [
                ["config", "user.email", "astdiff@csr.invalid"],
                ["config", "user.name", "CSR AstDiff Fixture"],
                ["config", "commit.gpgsign", "false"],
            ] {
                assert!(git_in(repo).args(args).status().unwrap().success());
            }
            Self { dir }
        }

        fn root(&self) -> &Path {
            self.dir.path()
        }

        fn write(&self, name: &str, contents: impl AsRef<[u8]>) {
            let path = self.root().join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, contents).unwrap();
        }

        fn remove(&self, name: &str) {
            std::fs::remove_file(self.root().join(name)).unwrap();
        }

        /// Stage everything and commit; returns the real commit OID.
        fn commit(&self, message: &str) -> String {
            let repo = self.root();
            assert!(git_in(repo).args(["add", "-A"]).status().unwrap().success());
            assert!(git_in(repo)
                .args(["commit", "-q", "--allow-empty", "-m", message])
                .status()
                .unwrap()
                .success());
            let out = git_in(repo).args(["rev-parse", "HEAD"]).output().unwrap();
            assert!(out.status.success());
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        }

        fn request(&self, path: &str, before: &str, after: &str) -> AstDiffRequest {
            AstDiffRequest {
                repo_root: self.root().to_path_buf(),
                path: path.to_string(),
                before_oid: before.to_string(),
                after_oid: after.to_string(),
            }
        }
    }

    fn diffed(outcome: &AstDiffOutcome) -> &AstDiff {
        match outcome {
            AstDiffOutcome::Diffed(diff) => diff,
            AstDiffOutcome::Abstained(reason) => {
                panic!("expected a diff, abstained: {}", reason.sentence())
            }
        }
    }

    fn abstained(outcome: &AstDiffOutcome) -> &Abstention {
        match outcome {
            AstDiffOutcome::Abstained(reason) => reason,
            AstDiffOutcome::Diffed(diff) => panic!(
                "expected an abstention, got a diff with {:?} and {} before / {} after nodes",
                diff.counts,
                diff.before.len(),
                diff.after.len()
            ),
        }
    }

    fn names(nodes: &[SymbolNode]) -> Vec<&str> {
        nodes.iter().map(|n| n.name.as_str()).collect()
    }

    fn status_of(nodes: &[SymbolNode], name: &str) -> SymbolStatus {
        nodes
            .iter()
            .find(|n| n.name == name)
            .unwrap_or_else(|| panic!("no node named {name} in {:?}", names(nodes)))
            .status
    }

    // --- diff cases -----------------------------------------------------

    #[test]
    fn symbol_added_is_added_and_the_survivor_stays_intact() {
        let fx = Fixture::new();
        fx.write("src/auth.rs", "fn keep() -> u8 { 1 }\n");
        let before = fx.commit("one");
        fx.write("src/auth.rs", "fn keep() -> u8 { 1 }\n\nfn fresh() {}\n");
        let after = fx.commit("two");

        let outcome = compute(&fx.request("src/auth.rs", &before, &after));
        let diff = diffed(&outcome);
        assert!(diff.before_present && diff.after_present);
        assert_eq!(names(&diff.before), vec!["keep"]);
        assert_eq!(names(&diff.after), vec!["keep", "fresh"]);
        assert_eq!(status_of(&diff.after, "fresh"), SymbolStatus::Added);
        assert_eq!(status_of(&diff.before, "keep"), SymbolStatus::Intact);
        assert_eq!(
            diff.counts,
            DiffCounts {
                intact: 1,
                changed: 0,
                removed: 0,
                added: 1
            }
        );
        assert_eq!(diff.connectors.len(), 1);
        assert_eq!(
            diff.connectors[0],
            Connector {
                before: 0,
                after: 0,
                changed: false
            }
        );
    }

    #[test]
    fn symbol_removed_is_removed() {
        let fx = Fixture::new();
        fx.write("src/auth.rs", "fn keep() {}\nfn doomed() {}\n");
        let before = fx.commit("one");
        fx.write("src/auth.rs", "fn keep() {}\n");
        let after = fx.commit("two");

        let outcome = compute(&fx.request("src/auth.rs", &before, &after));
        let diff = diffed(&outcome);
        assert_eq!(status_of(&diff.before, "doomed"), SymbolStatus::Removed);
        assert!(diff
            .before
            .iter()
            .find(|n| n.name == "doomed")
            .unwrap()
            .paired_with
            .is_none());
        assert_eq!(
            diff.counts,
            DiffCounts {
                intact: 1,
                changed: 0,
                removed: 1,
                added: 0
            }
        );
    }

    #[test]
    fn a_rename_is_removed_plus_added_and_never_claimed_as_a_rename() {
        let fx = Fixture::new();
        fx.write("src/auth.rs", "fn old_name() -> u8 { 7 }\n");
        let before = fx.commit("one");
        // Identical body, different name. Even that is not enough evidence
        // to assert the two are the same symbol.
        fx.write("src/auth.rs", "fn new_name() -> u8 { 7 }\n");
        let after = fx.commit("two");

        let outcome = compute(&fx.request("src/auth.rs", &before, &after));
        let diff = diffed(&outcome);
        assert_eq!(status_of(&diff.before, "old_name"), SymbolStatus::Removed);
        assert_eq!(status_of(&diff.after, "new_name"), SymbolStatus::Added);
        assert!(diff.connectors.is_empty(), "a rename must not be connected");
        assert_eq!(
            diff.counts,
            DiffCounts {
                intact: 0,
                changed: 0,
                removed: 1,
                added: 1
            }
        );
    }

    #[test]
    fn same_name_changed_body_is_changed_with_a_connector() {
        let fx = Fixture::new();
        fx.write("src/auth.rs", "fn check(t: &str) -> bool { t.len() > 8 }\n");
        let before = fx.commit("one");
        fx.write(
            "src/auth.rs",
            "fn check(t: &str) -> bool { t.len() > 12 }\n",
        );
        let after = fx.commit("two");

        let outcome = compute(&fx.request("src/auth.rs", &before, &after));
        let diff = diffed(&outcome);
        assert_eq!(status_of(&diff.before, "check"), SymbolStatus::Changed);
        assert_eq!(status_of(&diff.after, "check"), SymbolStatus::Changed);
        assert_eq!(diff.connectors.len(), 1);
        assert!(diff.connectors[0].changed);
        assert_eq!(diff.counts.changed, 1);
        assert_eq!(diff.counts.intact, 0);
    }

    #[test]
    fn reformatting_alone_reads_intact() {
        let fx = Fixture::new();
        fx.write("src/auth.rs", "fn check(t:&str)->bool{t.len()>8}\n");
        let before = fx.commit("one");
        fx.write(
            "src/auth.rs",
            "fn check(t: &str) -> bool {\n    t.len() > 8\n}\n",
        );
        let after = fx.commit("two");

        let diff = compute(&fx.request("src/auth.rs", &before, &after));
        let diff = diffed(&diff);
        assert_eq!(diff.counts.intact, 1);
        assert_eq!(diff.counts.changed, 0);
    }

    #[test]
    fn file_absent_before_reports_creation_not_an_abstention() {
        let fx = Fixture::new();
        fx.write("README.md", "seed\n");
        let before = fx.commit("no rust yet");
        fx.write("src/new.rs", "fn born() {}\n");
        let after = fx.commit("added");

        let outcome = compute(&fx.request("src/new.rs", &before, &after));
        let diff = diffed(&outcome);
        assert!(!diff.before_present, "file did not exist at the before oid");
        assert!(diff.after_present);
        assert!(diff.before.is_empty());
        assert_eq!(status_of(&diff.after, "born"), SymbolStatus::Added);
        assert_eq!(diff.counts.added, 1);
        assert_eq!(diff.counts.removed, 0);
    }

    #[test]
    fn file_absent_after_reports_deletion_not_an_abstention() {
        let fx = Fixture::new();
        fx.write("src/gone.rs", "fn vanishing() {}\n");
        let before = fx.commit("one");
        fx.remove("src/gone.rs");
        let after = fx.commit("deleted");

        let outcome = compute(&fx.request("src/gone.rs", &before, &after));
        let diff = diffed(&outcome);
        assert!(diff.before_present);
        assert!(!diff.after_present, "file was deleted at the after oid");
        assert!(diff.after.is_empty());
        assert_eq!(status_of(&diff.before, "vanishing"), SymbolStatus::Removed);
        assert_eq!(diff.counts.removed, 1);
    }

    #[test]
    fn impl_members_are_qualified_so_same_named_methods_do_not_cross_pair() {
        let fx = Fixture::new();
        fx.write(
            "src/pair.rs",
            "struct A; struct B;\nimpl A { fn new() -> u8 { 1 } }\nimpl B { fn new() -> u8 { 2 } }\n",
        );
        let before = fx.commit("one");
        fx.write(
            "src/pair.rs",
            "struct A; struct B;\nimpl A { fn new() -> u8 { 1 } }\nimpl B { fn new() -> u8 { 99 } }\n",
        );
        let after = fx.commit("two");

        let outcome = compute(&fx.request("src/pair.rs", &before, &after));
        let diff = diffed(&outcome);
        assert_eq!(status_of(&diff.before, "A::new"), SymbolStatus::Intact);
        assert_eq!(status_of(&diff.before, "B::new"), SymbolStatus::Changed);
        let member = diff.before.iter().find(|n| n.name == "B::new").unwrap();
        assert_eq!(member.container.as_deref(), Some("B"));
        assert_eq!(member.depth, 1);
        let top = diff.before.iter().find(|n| n.name == "A").unwrap();
        assert_eq!(top.depth, 0);
        assert_eq!(top.container, None);
    }

    #[test]
    fn source_order_is_preserved_on_both_sides() {
        let fx = Fixture::new();
        fx.write("src/order.rs", "fn a() {}\nfn b() {}\nfn c() {}\n");
        let before = fx.commit("one");
        fx.write("src/order.rs", "fn c() {}\nfn a() {}\nfn b() {}\n");
        let after = fx.commit("two");

        let outcome = compute(&fx.request("src/order.rs", &before, &after));
        let diff = diffed(&outcome);
        assert_eq!(names(&diff.before), vec!["a", "b", "c"]);
        assert_eq!(names(&diff.after), vec!["c", "a", "b"]);
        assert!(diff.before.windows(2).all(|w| w[0].line <= w[1].line));
        // Connectors are emitted in before order but point at the moved
        // after-side indices.
        assert_eq!(
            diff.connectors,
            vec![
                Connector {
                    before: 0,
                    after: 1,
                    changed: false
                },
                Connector {
                    before: 1,
                    after: 2,
                    changed: false
                },
                Connector {
                    before: 2,
                    after: 0,
                    changed: false
                },
            ]
        );
        assert_eq!(diff.counts.intact, 3);
    }

    #[test]
    fn typescript_classes_and_functions_both_diff() {
        let fx = Fixture::new();
        fx.write(
            "web/app.ts",
            "export class Store { load() { return 1; } }\nfunction helper() { return 2; }\n",
        );
        let before = fx.commit("one");
        fx.write(
            "web/app.ts",
            "export class Store { load() { return 5; } }\n",
        );
        let after = fx.commit("two");

        let outcome = compute(&fx.request("web/app.ts", &before, &after));
        let diff = diffed(&outcome);
        assert_eq!(diff.language, "TypeScript");
        assert_eq!(status_of(&diff.before, "helper"), SymbolStatus::Removed);
        assert_eq!(
            status_of(&diff.before, "Store::load"),
            SymbolStatus::Changed
        );
    }

    // --- abstention paths ------------------------------------------------

    #[test]
    fn binary_blob_abstains_as_not_utf8() {
        let fx = Fixture::new();
        fx.write("src/bin.rs", "fn real() {}\n");
        let before = fx.commit("one");
        fx.write("src/bin.rs", [0x00u8, 0xff, 0xfe, 0x00, 0x80, 0x01]);
        let after = fx.commit("binary now");

        let outcome = compute(&fx.request("src/bin.rs", &before, &after));
        match abstained(&outcome) {
            Abstention::NotUtf8 { side, path } => {
                assert_eq!(*side, Side::After);
                assert_eq!(path, "src/bin.rs");
            }
            other => panic!("expected NotUtf8, got {other:?}"),
        }
        assert!(abstained(&outcome).sentence().contains("not valid UTF-8"));
    }

    #[test]
    fn unknown_oid_abstains_and_names_the_oid() {
        let fx = Fixture::new();
        fx.write("src/a.rs", "fn a() {}\n");
        let before = fx.commit("one");
        let missing = "0123456789abcdef0123456789abcdef01234567";

        let outcome = compute(&fx.request("src/a.rs", &before, missing));
        match abstained(&outcome) {
            Abstention::UnknownOid { side, oid, .. } => {
                assert_eq!(*side, Side::After);
                assert_eq!(oid, missing);
            }
            other => panic!("expected UnknownOid, got {other:?}"),
        }
        assert!(abstained(&outcome).sentence().contains(missing));
        assert!(
            !abstained(&outcome).is_stable(),
            "a fetch could supply this commit later; it must not be cached"
        );
    }

    #[test]
    fn malformed_oid_never_reaches_git() {
        let fx = Fixture::new();
        fx.write("src/a.rs", "fn a() {}\n");
        let real = fx.commit("one");

        for hostile in ["HEAD", "main@{1}", "", "zz", "../../etc/passwd", "a..b"] {
            let outcome = compute(&fx.request("src/a.rs", hostile, &real));
            match abstained(&outcome) {
                Abstention::MalformedOid { side, oid } => {
                    assert_eq!(*side, Side::Before);
                    assert_eq!(oid, hostile);
                }
                other => panic!("{hostile:?} should be MalformedOid, got {other:?}"),
            }
        }
    }

    #[test]
    fn escaping_or_absolute_paths_are_rejected_before_any_git_access() {
        let fx = Fixture::new();
        fx.write("src/a.rs", "fn a() {}\n");
        let one = fx.commit("one");

        for hostile in ["../outside.rs", "/etc/hosts", "  ", "a/../../b.rs"] {
            let outcome = compute(&fx.request(hostile, &one, &one));
            assert!(
                matches!(abstained(&outcome), Abstention::PathRejected { .. }),
                "{hostile:?} should be PathRejected, got {:?}",
                abstained(&outcome)
            );
        }
    }

    #[test]
    fn unsupported_extension_abstains_with_the_extension() {
        let fx = Fixture::new();
        fx.write("ios/Player.swift", "class Player { func show() {} }\n");
        let before = fx.commit("one");
        fx.write("ios/Player.swift", "class Player { func hide() {} }\n");
        let after = fx.commit("two");

        let outcome = compute(&fx.request("ios/Player.swift", &before, &after));
        match abstained(&outcome) {
            Abstention::UnsupportedLanguage { path, extension } => {
                assert_eq!(path, "ios/Player.swift");
                assert_eq!(extension, "swift");
            }
            other => panic!("expected UnsupportedLanguage, got {other:?}"),
        }
    }

    #[test]
    fn absent_from_both_sides_abstains_rather_than_returning_an_empty_diff() {
        let fx = Fixture::new();
        fx.write("src/a.rs", "fn a() {}\n");
        let before = fx.commit("one");
        fx.write("src/b.rs", "fn b() {}\n");
        let after = fx.commit("two");

        let outcome = compute(&fx.request("src/never.rs", &before, &after));
        match abstained(&outcome) {
            Abstention::AbsentFromBothSides {
                path,
                before_oid,
                after_oid,
            } => {
                assert_eq!(path, "src/never.rs");
                assert_eq!(before_oid, &before);
                assert_eq!(after_oid, &after);
            }
            other => panic!("expected AbsentFromBothSides, got {other:?}"),
        }
    }

    #[test]
    fn a_file_with_no_named_symbols_abstains_instead_of_reading_as_unchanged() {
        let fx = Fixture::new();
        fx.write("src/only_comments.rs", "// before\nmod other;\n");
        let before = fx.commit("one");
        fx.write(
            "src/only_comments.rs",
            "// after, definitely different\nmod other;\n",
        );
        let after = fx.commit("two");

        let outcome = compute(&fx.request("src/only_comments.rs", &before, &after));
        assert!(
            matches!(abstained(&outcome), Abstention::NoSymbolsExtracted { .. }),
            "got {:?}",
            abstained(&outcome)
        );
    }

    #[test]
    fn unparseable_garbage_abstains_rather_than_inventing_symbols() {
        let fx = Fixture::new();
        // Valid UTF-8, structurally meaningless Rust: tree-sitter recovers
        // no named definition, so there is nothing honest to show.
        fx.write("src/garbage.rs", ")))} <<< ??? &&& (((\n");
        let before = fx.commit("one");
        fx.write("src/garbage.rs", "@@@@ ]]]] ;;;; ####\n");
        let after = fx.commit("two");

        let outcome = compute(&fx.request("src/garbage.rs", &before, &after));
        assert!(
            matches!(abstained(&outcome), Abstention::NoSymbolsExtracted { .. }),
            "got {:?}",
            abstained(&outcome)
        );
    }

    #[test]
    fn oversized_blob_abstains_with_the_measured_size() {
        let fx = Fixture::new();
        fx.write("src/big.rs", "fn small() {}\n");
        let before = fx.commit("one");
        let mut huge = String::from("fn small() {}\n");
        while huge.len() <= MAX_BLOB_BYTES {
            huge.push_str("// padding padding padding padding padding padding\n");
        }
        fx.write("src/big.rs", &huge);
        let after = fx.commit("two");

        let outcome = compute(&fx.request("src/big.rs", &before, &after));
        match abstained(&outcome) {
            Abstention::FileTooLarge {
                side, bytes, limit, ..
            } => {
                assert_eq!(*side, Side::After);
                assert_eq!(*bytes, huge.len());
                assert_eq!(*limit, MAX_BLOB_BYTES);
            }
            other => panic!("expected FileTooLarge, got {other:?}"),
        }
    }

    /// Total bytes of everything under `.git/objects`, i.e. the *stored*
    /// (zlib-compressed) size of the repository's object database.
    fn odb_bytes(repo: &Path) -> u64 {
        fn walk(dir: &Path, total: &mut u64) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let Ok(meta) = entry.metadata() else { continue };
                if meta.is_dir() {
                    walk(&entry.path(), total);
                } else {
                    *total += meta.len();
                }
            }
        }
        let mut total = 0;
        walk(&repo.join(".git").join("objects"), &mut total);
        total
    }

    /// Finding 7. The guard has to decide from the blob's declared size,
    /// not from the length of a `Vec<u8>` that has already been allocated.
    #[test]
    fn an_oversized_blob_is_refused_from_its_header_before_it_is_materialised() {
        let fx = Fixture::new();
        fx.write("src/bomb.rs", "fn small() {}\n");
        let before = fx.commit("one");

        // 8 MiB — sixteen times the guard — and pathologically
        // compressible, so git stores it in a few kilobytes. This is the
        // review's "very large compressed git blob": a post-allocation
        // check would have had to inflate all 8 MiB on every request just
        // to discover the file was too big to parse.
        const DECLARED: usize = 8 * 1024 * 1024;
        let mut bomb = String::with_capacity(DECLARED + 32);
        bomb.push_str("fn small() {}\n");
        while bomb.len() < DECLARED {
            bomb.push('A');
        }
        fx.write("src/bomb.rs", &bomb);
        let after = fx.commit("bomb");

        let auditor = codewitness::Auditor::open(fx.root().to_path_buf()).unwrap();
        let after_commit = auditor.resolve_commit(&after).unwrap();

        // Metadata alone knows the size, and knows it is over the cap.
        let plan = inspect_blob(
            &auditor,
            Path::new("src/bomb.rs"),
            "src/bomb.rs",
            after_commit,
            Side::After,
        )
        .expect("the header must be readable");
        assert_eq!(
            plan,
            BlobPlan::Blob {
                bytes: bomb.len() as u64
            }
        );
        assert!(bomb.len() > MAX_BLOB_BYTES);

        // ...and the entire object database is smaller than the cap, so the
        // declared size provably did not come from measuring what was read:
        // there are not 8 MiB anywhere on disk to have read.
        let stored = odb_bytes(fx.root());
        assert!(
            stored < MAX_BLOB_BYTES as u64,
            "the whole odb is {stored} bytes while the blob declares {} — \
             the size must have come from the header, not from content",
            bomb.len()
        );

        // The guard path refuses it, with the declared size and the limit.
        let refused = read_side(
            &auditor,
            Path::new("src/bomb.rs"),
            "src/bomb.rs",
            after_commit,
            Side::After,
        )
        .expect_err("an oversized blob must not be read");
        match &refused {
            Abstention::FileTooLarge {
                side, bytes, limit, ..
            } => {
                assert_eq!(*side, Side::After);
                assert_eq!(*bytes, bomb.len());
                assert_eq!(*limit, MAX_BLOB_BYTES);
            }
            other => panic!("expected FileTooLarge, got {other:?}"),
        }

        // End to end: an explicit abstention, never an empty tree.
        let outcome = compute(&fx.request("src/bomb.rs", &before, &after));
        assert!(outcome.diff().is_none());
        assert!(abstained(&outcome)
            .sentence()
            .contains("over the 524288-byte parse limit"));
    }

    /// A blob that fits is still read, and the header size it was admitted
    /// on is the real content length — the guard is a bound, not a wall.
    #[test]
    fn a_blob_under_the_cap_reports_its_exact_size_and_is_read() {
        let fx = Fixture::new();
        let source = "fn fits() {}\n";
        fx.write("src/fits.rs", source);
        let one = fx.commit("one");

        let auditor = codewitness::Auditor::open(fx.root().to_path_buf()).unwrap();
        let commit = auditor.resolve_commit(&one).unwrap();
        assert_eq!(
            inspect_blob(
                &auditor,
                Path::new("src/fits.rs"),
                "src/fits.rs",
                commit,
                Side::Before
            )
            .unwrap(),
            BlobPlan::Blob {
                bytes: source.len() as u64
            }
        );
        assert!(matches!(
            read_side(
                &auditor,
                Path::new("src/fits.rs"),
                "src/fits.rs",
                commit,
                Side::Before
            )
            .unwrap(),
            SideRead::Present(_)
        ));
    }

    /// An absent path is reported as absence by metadata inspection too —
    /// the "created / deleted" fact must not become an abstention.
    #[test]
    fn a_path_absent_from_the_tree_inspects_as_absent_not_as_a_failure() {
        let fx = Fixture::new();
        fx.write("src/here.rs", "fn here() {}\n");
        let one = fx.commit("one");

        let auditor = codewitness::Auditor::open(fx.root().to_path_buf()).unwrap();
        let commit = auditor.resolve_commit(&one).unwrap();
        assert_eq!(
            inspect_blob(
                &auditor,
                Path::new("src/nowhere.rs"),
                "src/nowhere.rs",
                commit,
                Side::Before
            )
            .unwrap(),
            BlobPlan::Absent
        );
    }

    /// Git stores a symlink's target as an ordinary blob, so an
    /// object-level check would hand back `"real.rs"` as if it were the
    /// file's content. The entry mode is the only place the distinction
    /// survives, and this pins that it is the thing being checked.
    #[cfg(unix)]
    #[test]
    fn a_symlink_abstains_instead_of_diffing_its_target_string() {
        let fx = Fixture::new();
        fx.write("src/real.rs", "fn real() {}\n");
        std::os::unix::fs::symlink("real.rs", fx.root().join("src/link.rs")).unwrap();
        let one = fx.commit("a symlink beside a real file");

        let outcome = compute(&fx.request("src/link.rs", &one, &one));
        match abstained(&outcome) {
            Abstention::UnreadableBlob { side, path, detail } => {
                assert_eq!(*side, Side::Before);
                assert_eq!(path, "src/link.rs");
                assert!(detail.contains("symlink"), "detail was {detail:?}");
            }
            other => panic!("expected UnreadableBlob, got {other:?}"),
        }
    }

    #[test]
    fn a_directory_at_the_path_is_an_unreadable_blob_not_an_absence() {
        let fx = Fixture::new();
        fx.write("src/thing.rs/inner.rs", "fn inner() {}\n");
        let one = fx.commit("path is a directory");

        let outcome = compute(&fx.request("src/thing.rs", &one, &one));
        match abstained(&outcome) {
            Abstention::UnreadableBlob { side, detail, .. } => {
                assert_eq!(*side, Side::Before);
                assert!(detail.contains("tree"), "detail was {detail:?}");
            }
            other => panic!("expected UnreadableBlob, got {other:?}"),
        }
    }

    #[test]
    fn a_non_repository_root_abstains() {
        let dir = tempfile::tempdir().unwrap();
        let request = AstDiffRequest {
            repo_root: dir.path().to_path_buf(),
            path: "src/a.rs".to_string(),
            before_oid: "0123456789abcdef0123456789abcdef01234567".to_string(),
            after_oid: "89abcdef0123456789abcdef0123456789abcdef".to_string(),
        };
        let outcome = compute(&request);
        assert!(
            matches!(
                abstained(&outcome),
                Abstention::RepositoryUnavailable { .. }
            ),
            "got {:?}",
            abstained(&outcome)
        );
        assert!(!abstained(&outcome).is_stable());
    }

    /// The binding rule: no abstention may render as an empty tree, and
    /// every one must say why.
    #[test]
    fn every_abstention_variant_carries_a_reason_sentence() {
        let all = vec![
            Abstention::RepositoryUnavailable {
                repo_root: "/tmp/x".into(),
                detail: "not a repo".into(),
            },
            Abstention::PathRejected {
                path: "../x".into(),
                detail: "contains a `..` component".into(),
            },
            Abstention::MalformedOid {
                side: Side::Before,
                oid: "HEAD".into(),
            },
            Abstention::UnknownOid {
                side: Side::After,
                oid: "abcdef1".into(),
                detail: "no such commit".into(),
            },
            Abstention::UnsupportedLanguage {
                path: "a.swift".into(),
                extension: "swift".into(),
            },
            Abstention::FileTooLarge {
                side: Side::After,
                path: "a.rs".into(),
                bytes: 999_999,
                limit: MAX_BLOB_BYTES,
            },
            Abstention::UnreadableBlob {
                side: Side::Before,
                path: "a.rs".into(),
                detail: "odb error".into(),
            },
            Abstention::NotUtf8 {
                side: Side::After,
                path: "a.rs".into(),
            },
            Abstention::ParsePanicked {
                side: Side::Before,
                path: "a.rs".into(),
            },
            Abstention::AbsentFromBothSides {
                path: "a.rs".into(),
                before_oid: "aaaa".into(),
                after_oid: "bbbb".into(),
            },
            Abstention::NoSymbolsExtracted {
                path: "a.rs".into(),
                before_oid: "aaaa".into(),
                after_oid: "bbbb".into(),
            },
        ];
        for reason in all {
            let sentence = reason.sentence();
            assert!(
                sentence.starts_with("AST comparison abstained:"),
                "{reason:?} must lead with the abstention, got {sentence:?}"
            );
            assert!(sentence.len() > 40, "{reason:?} gave a stub: {sentence:?}");
            assert!(
                !sentence.contains("{"),
                "{reason:?} left an unformatted placeholder: {sentence:?}"
            );
        }
    }

    /// The abstention contract swept end to end over **real** fixture
    /// repositories: every failure mode the engine can actually reach must
    /// come back `Abstained`, with a reason, and with no diff attached —
    /// so nothing downstream can render it as "nothing changed".
    ///
    /// Ten of the eleven variants are produced here from live inputs.
    /// [`Abstention::ParsePanicked`] is the exception and is stated rather
    /// than claimed: it exists only for a tree-sitter panic, which cannot
    /// be provoked from source text, so it is covered by
    /// `every_abstention_variant_carries_a_reason_sentence` (its sentence)
    /// and by construction (`extract_symbols` returns `None` only from
    /// `catch_unwind`) — not by an executed panic.
    #[test]
    fn every_reachable_failure_mode_abstains_with_a_reason_and_no_diff() {
        let fx = Fixture::new();
        fx.write("src/a.rs", "fn a() {}\n");
        fx.write("src/only_comments.rs", "// nothing but a comment\n");
        fx.write("ios/P.swift", "class P {}\n");
        fx.write("src/dir.rs/inner.rs", "fn inner() {}\n");
        let one = fx.commit("one");
        fx.write("src/a.rs", "fn a() { let x = 1; }\n");
        fx.write("src/binary.rs", [0x00u8, 0xff, 0xfe, 0x80]);
        let mut huge = String::from("fn big() {}\n");
        while huge.len() <= MAX_BLOB_BYTES {
            huge.push_str("// padding padding padding padding padding\n");
        }
        fx.write("src/huge.rs", &huge);
        let two = fx.commit("two");

        /// One case: a label, the request that provokes it, and the
        /// predicate naming the exact variant it must produce.
        type Case = (&'static str, AstDiffRequest, fn(&Abstention) -> bool);

        let elsewhere = tempfile::tempdir().unwrap();
        let cases: Vec<Case> = vec![
            (
                "repository unavailable",
                AstDiffRequest {
                    repo_root: elsewhere.path().to_path_buf(),
                    path: "src/a.rs".into(),
                    before_oid: one.clone(),
                    after_oid: two.clone(),
                },
                |r| matches!(r, Abstention::RepositoryUnavailable { .. }),
            ),
            (
                "path rejected",
                fx.request("../escape.rs", &one, &two),
                |r| matches!(r, Abstention::PathRejected { .. }),
            ),
            ("malformed oid", fx.request("src/a.rs", "HEAD", &two), |r| {
                matches!(r, Abstention::MalformedOid { .. })
            }),
            (
                "unknown oid",
                fx.request("src/a.rs", &one, "0123456789abcdef0123456789abcdef01234567"),
                |r| matches!(r, Abstention::UnknownOid { .. }),
            ),
            (
                "unsupported language",
                fx.request("ios/P.swift", &one, &two),
                |r| matches!(r, Abstention::UnsupportedLanguage { .. }),
            ),
            (
                "file too large",
                fx.request("src/huge.rs", &one, &two),
                |r| matches!(r, Abstention::FileTooLarge { .. }),
            ),
            (
                "unreadable blob",
                fx.request("src/dir.rs", &one, &two),
                |r| matches!(r, Abstention::UnreadableBlob { .. }),
            ),
            ("not utf-8", fx.request("src/binary.rs", &one, &two), |r| {
                matches!(r, Abstention::NotUtf8 { .. })
            }),
            (
                "absent from both sides",
                fx.request("src/never.rs", &one, &two),
                |r| matches!(r, Abstention::AbsentFromBothSides { .. }),
            ),
            (
                "no symbols extracted",
                fx.request("src/only_comments.rs", &one, &two),
                |r| matches!(r, Abstention::NoSymbolsExtracted { .. }),
            ),
        ];

        for (label, request, expected) in cases {
            let outcome = compute(&request);
            assert!(
                outcome.diff().is_none(),
                "{label}: an abstention must carry no diff"
            );
            let reason = outcome
                .abstention()
                .unwrap_or_else(|| panic!("{label}: expected an abstention"));
            assert!(expected(reason), "{label}: got {reason:?}");
            let sentence = reason.sentence();
            assert!(
                sentence.starts_with("AST comparison abstained:"),
                "{label}: {sentence:?}"
            );
            assert!(sentence.len() > 40, "{label}: stub sentence {sentence:?}");
            // The cached form must round-trip to the same explicit reason.
            let cache = AstDiffCache::new();
            assert!(
                cache.get_or_compute(&request).diff().is_none(),
                "{label}: the cache must not turn an abstention into a diff"
            );
        }
    }

    /// The other half of the contract: whenever the engine *does* return a
    /// diff, at least one side carries a symbol. An empty tree is never a
    /// result, so a renderer can trust that `Diffed` means something to
    /// draw.
    #[test]
    fn a_diffed_outcome_always_carries_at_least_one_symbol() {
        let fx = Fixture::new();
        fx.write("src/a.rs", "fn a() {}\n");
        fx.write("src/keep.rs", "fn keep() {}\n");
        fx.write("src/gone.rs", "fn gone() {}\n");
        let one = fx.commit("one");
        fx.write("src/a.rs", "fn a() { let x = 1; }\n");
        fx.write("src/born.rs", "fn born() {}\n");
        fx.remove("src/gone.rs");
        let two = fx.commit("two");

        for path in ["src/a.rs", "src/keep.rs", "src/gone.rs", "src/born.rs"] {
            let outcome = compute(&fx.request(path, &one, &two));
            let diff = diffed(&outcome);
            assert!(
                !(diff.before.is_empty() && diff.after.is_empty()),
                "{path}: a diff with two empty sides would render as `nothing changed`"
            );
            assert!(
                diff.before_present || diff.after_present,
                "{path}: a diff must have existed on at least one side"
            );
        }
    }

    // --- churn -----------------------------------------------------------

    #[test]
    fn churn_is_measured_or_absent_and_never_a_fabricated_zero() {
        let fx = Fixture::new();
        fx.write("src/hot.rs", "fn hot() { }\n");
        let before = fx.commit("one");
        fx.write("src/hot.rs", "fn hot() { let x = 1; }\n");
        let after = fx.commit("two");

        let bare = compute(&fx.request("src/hot.rs", &before, &after));
        let bare_diff = diffed(&bare);
        assert_eq!(bare_diff.churn, ChurnTint::Unmeasured);
        assert_eq!(bare_diff.churn.touches(), None);
        assert!(bare_diff.before.iter().all(|n| n.churn.touches().is_none()));
        assert_eq!(
            serde_json::to_string(&bare_diff.churn).unwrap(),
            "null",
            "an unmeasured tint must serialize to null, never to a number"
        );

        let tiles = vec![
            ChurnTile {
                file: "/Users/someone/proj/src/hot.rs".into(),
                touches: 7,
            },
            ChurnTile {
                file: "src/cold.rs".into(),
                touches: 3,
            },
        ];
        let tinted = compute(&fx.request("src/hot.rs", &before, &after)).with_churn(&tiles);
        let tinted_diff = diffed(&tinted);
        assert_eq!(tinted_diff.churn.touches(), Some(7));
        assert!(tinted_diff
            .before
            .iter()
            .all(|n| n.churn.touches() == Some(7)));
        assert_eq!(serde_json::to_string(&tinted_diff.churn).unwrap(), "7");

        // A file with no measured row stays Unmeasured — absence never
        // becomes zero.
        let unmeasured =
            compute(&fx.request("src/hot.rs", &before, &after)).with_churn(&[ChurnTile {
                file: "src/elsewhere.rs".into(),
                touches: 9,
            }]);
        assert_eq!(diffed(&unmeasured).churn, ChurnTint::Unmeasured);
    }

    #[test]
    fn a_measured_zero_is_distinguishable_from_no_measurement() {
        let measured = ChurnTint::from_churn(
            "src/quiet.rs",
            &[ChurnTile {
                file: "src/quiet.rs".into(),
                touches: 0,
            }],
        );
        assert!(measured.is_measured());
        assert_eq!(measured.touches(), Some(0));
        assert_eq!(serde_json::to_string(&measured).unwrap(), "0");

        let absent = ChurnTint::from_churn("src/quiet.rs", &[]);
        assert!(!absent.is_measured());
        assert_eq!(absent.touches(), None);
        assert_eq!(ChurnTint::default(), ChurnTint::Unmeasured);
    }

    // --- truncation -------------------------------------------------------

    #[test]
    fn truncation_discloses_the_measured_total() {
        let fx = Fixture::new();
        let mut source = String::new();
        for index in 0..(MAX_SYMBOLS_PER_SIDE + 20) {
            source.push_str(&format!("fn f{index}() {{}}\n"));
        }
        fx.write("src/many.rs", &source);
        let before = fx.commit("one");
        source.push_str("fn extra() {}\n");
        fx.write("src/many.rs", &source);
        let after = fx.commit("two");

        let outcome = compute(&fx.request("src/many.rs", &before, &after));
        let diff = diffed(&outcome);
        assert_eq!(diff.before.len(), MAX_SYMBOLS_PER_SIDE);
        assert_eq!(diff.before_total, MAX_SYMBOLS_PER_SIDE + 20);
        assert_eq!(diff.after_total, MAX_SYMBOLS_PER_SIDE + 21);
        assert!(diff.truncated(), "a truncated tree must say so");
    }

    // --- cache ------------------------------------------------------------

    #[test]
    fn cache_hits_the_second_time_and_evicts_least_recently_used() {
        let fx = Fixture::new();
        fx.write("src/a.rs", "fn a() {}\n");
        fx.write("src/b.rs", "fn b() {}\n");
        fx.write("src/c.rs", "fn c() {}\n");
        let before = fx.commit("one");
        fx.write("src/a.rs", "fn a() { let x = 1; }\n");
        fx.write("src/b.rs", "fn b() { let x = 2; }\n");
        fx.write("src/c.rs", "fn c() { let x = 3; }\n");
        let after = fx.commit("two");

        let cache = AstDiffCache::with_limits(2, DEFAULT_CACHE_BYTES);
        let a = fx.request("src/a.rs", &before, &after);
        let b = fx.request("src/b.rs", &before, &after);
        let c = fx.request("src/c.rs", &before, &after);

        let first = cache.get_or_compute(&a);
        assert_eq!(cache.stats().misses, 1);
        assert_eq!(cache.stats().hits, 0);

        let second = cache.get_or_compute(&a);
        assert_eq!(cache.stats().hits, 1, "identical key must hit");
        assert!(
            Arc::ptr_eq(&first, &second),
            "a hit must return the stored Arc"
        );

        cache.get_or_compute(&b);
        assert_eq!(cache.stats().entries, 2);
        assert!(cache.stats().bytes > 0);

        // An insert counts as a use, so `b` is currently the more recent of
        // the two. Read `a` again to make it the most recently used; `c`
        // must then evict `b`.
        cache.get_or_compute(&a);
        assert_eq!(cache.stats().hits, 2);

        cache.get_or_compute(&c);
        assert_eq!(cache.stats().entries, 2, "entry cap must hold");
        assert_eq!(cache.stats().evictions, 1);

        cache.get_or_compute(&a);
        assert_eq!(cache.stats().hits, 3, "`a` must have survived");
        cache.get_or_compute(&b);
        assert_eq!(cache.stats().hits, 3, "`b` must have been evicted");
    }

    #[test]
    fn byte_cap_evicts_even_below_the_entry_cap() {
        let fx = Fixture::new();
        fx.write("src/a.rs", "fn a() {}\n");
        fx.write("src/b.rs", "fn b() {}\n");
        let before = fx.commit("one");
        fx.write("src/a.rs", "fn a() { let x = 1; }\n");
        fx.write("src/b.rs", "fn b() { let x = 2; }\n");
        let after = fx.commit("two");

        let a = fx.request("src/a.rs", &before, &after);
        let single = approx_bytes(&compute(&a));
        // Room for one entry, not two.
        let cache = AstDiffCache::with_limits(64, single + 1);
        cache.get_or_compute(&a);
        assert_eq!(cache.stats().entries, 1);
        cache.get_or_compute(&fx.request("src/b.rs", &before, &after));
        assert_eq!(cache.stats().entries, 1, "byte cap must evict");
        assert!(cache.stats().bytes <= single + 1);
        assert_eq!(cache.stats().evictions, 1);
    }

    #[test]
    fn an_entry_larger_than_the_whole_budget_is_served_but_not_stored() {
        let fx = Fixture::new();
        fx.write("src/a.rs", "fn a() {}\n");
        let before = fx.commit("one");
        fx.write("src/a.rs", "fn a() { let x = 1; }\n");
        let after = fx.commit("two");

        let cache = AstDiffCache::with_limits(8, 1);
        let request = fx.request("src/a.rs", &before, &after);
        let outcome = cache.get_or_compute(&request);
        assert!(outcome.diff().is_some(), "the caller still gets the answer");
        assert_eq!(cache.stats().entries, 0);
        assert_eq!(cache.stats().bytes, 0);
        assert_eq!(cache.stats().rejected_oversize, 1);
    }

    /// Both bounds hold simultaneously while entries churn, and the byte
    /// counter never drifts from what is actually stored — a counter that
    /// under-counts would let the cache grow past its budget silently.
    #[test]
    fn the_cache_holds_both_bounds_under_churn_and_its_byte_count_never_drifts() {
        let fx = Fixture::new();
        let files: Vec<String> = (0..8).map(|i| format!("src/f{i}.rs")).collect();
        for (index, file) in files.iter().enumerate() {
            fx.write(file, format!("fn f{index}() {{}}\n"));
        }
        let before = fx.commit("one");
        for (index, file) in files.iter().enumerate() {
            fx.write(file, format!("fn f{index}() {{ let x = {index}; }}\n"));
        }
        let after = fx.commit("two");

        let one_entry = approx_bytes(&compute(&fx.request(&files[0], &before, &after)));
        // Room for three entries by count, but only ~two by bytes: whichever
        // bound bites first must bite.
        let max_entries = 3;
        let max_bytes = one_entry * 2 + 1;
        let cache = AstDiffCache::with_limits(max_entries, max_bytes);

        // Two passes over eight distinct keys, so entries are inserted,
        // evicted, and re-inserted.
        for _ in 0..2 {
            for file in &files {
                cache.get_or_compute(&fx.request(file, &before, &after));
                let stats = cache.stats();
                assert!(
                    stats.entries <= max_entries,
                    "entry cap breached: {stats:?} after {file}"
                );
                assert!(
                    stats.bytes <= max_bytes,
                    "byte cap breached: {stats:?} after {file}"
                );
                let stored: usize = {
                    let inner = cache.lock();
                    inner.entries.values().map(|entry| entry.bytes).sum()
                };
                assert_eq!(
                    stored, stats.bytes,
                    "the byte counter drifted from what is stored, after {file}"
                );
            }
        }
        assert!(
            cache.stats().evictions > 0,
            "eight keys through a two-entry budget must have evicted"
        );
        assert!(
            cache.stats().entries <= 2,
            "the byte cap is the tighter one"
        );
    }

    /// Re-inserting the same key replaces rather than accumulates. Without
    /// the subtraction in `insert`, a hot key would inflate `bytes` on every
    /// recompute and evict the whole cache.
    #[test]
    fn reinserting_the_same_key_does_not_double_count_its_bytes() {
        let fx = Fixture::new();
        fx.write("src/a.rs", "fn a() {}\n");
        let before = fx.commit("one");
        fx.write("src/a.rs", "fn a() { let x = 1; }\n");
        let after = fx.commit("two");

        let request = fx.request("src/a.rs", &before, &after);
        let key = CacheKey::of(&request);
        let value = Arc::new(compute(&request));
        let cache = AstDiffCache::new();
        for _ in 0..5 {
            cache.insert(key.clone(), value.clone());
        }
        let stats = cache.stats();
        assert_eq!(stats.entries, 1);
        assert_eq!(stats.bytes, approx_bytes(value.as_ref()));
        assert_eq!(stats.evictions, 0);
    }

    #[test]
    fn availability_abstentions_are_recomputed_stable_ones_are_cached() {
        let fx = Fixture::new();
        fx.write("src/a.rs", "fn a() {}\n");
        let before = fx.commit("one");
        fx.write("src/a.rs", "fn a() { let x = 1; }\n");
        let after = fx.commit("two");
        let cache = AstDiffCache::new();

        // Unknown OID: availability-dependent, never cached.
        let unknown = fx.request(
            "src/a.rs",
            &before,
            "0123456789abcdef0123456789abcdef01234567",
        );
        cache.get_or_compute(&unknown);
        cache.get_or_compute(&unknown);
        assert_eq!(cache.stats().entries, 0);
        assert_eq!(cache.stats().hits, 0);
        assert_eq!(cache.stats().misses, 2);

        // Unsupported language: derived from the path alone, cached.
        fx.write("ios/P.swift", "class P {}\n");
        let swift_before = fx.commit("three");
        let swift = fx.request("ios/P.swift", &swift_before, &after);
        cache.get_or_compute(&swift);
        cache.get_or_compute(&swift);
        assert_eq!(cache.stats().hits, 1);
        assert_eq!(cache.stats().entries, 1);

        cache.clear();
        assert_eq!(cache.stats().entries, 0);
        assert_eq!(cache.stats().bytes, 0);
    }

    #[test]
    fn cache_key_separates_paths_oids_and_repositories() {
        let fx = Fixture::new();
        fx.write("src/a.rs", "fn a() {}\n");
        let one = fx.commit("one");
        fx.write("src/a.rs", "fn a() { let x = 1; }\n");
        let two = fx.commit("two");
        fx.write("src/a.rs", "fn a() { let x = 2; }\n");
        let three = fx.commit("three");

        let cache = AstDiffCache::new();
        cache.get_or_compute(&fx.request("src/a.rs", &one, &two));
        cache.get_or_compute(&fx.request("src/a.rs", &one, &three));
        cache.get_or_compute(&fx.request("src/a.rs", &two, &three));
        assert_eq!(cache.stats().entries, 3, "each oid pair is its own entry");
        assert_eq!(cache.stats().hits, 0);

        let other = Fixture::new();
        other.write("src/a.rs", "fn a() {}\n");
        let other_one = other.commit("one");
        other.write("src/a.rs", "fn a() { let x = 1; }\n");
        let other_two = other.commit("two");
        cache.get_or_compute(&other.request("src/a.rs", &other_one, &other_two));
        assert_eq!(
            cache.stats().entries,
            4,
            "repo root separates identical paths"
        );
    }

    // --- serialization ----------------------------------------------------

    #[test]
    fn an_outcome_serializes_with_its_variant_named() {
        let fx = Fixture::new();
        fx.write("src/a.rs", "fn a() {}\n");
        let before = fx.commit("one");
        fx.write("src/a.rs", "fn a() { let x = 1; }\n");
        let after = fx.commit("two");

        let json =
            serde_json::to_string(&compute(&fx.request("src/a.rs", &before, &after))).unwrap();
        assert!(json.contains("\"diffed\""), "{json}");
        assert!(json.contains("\"changed\""));

        let abstain =
            serde_json::to_string(&compute(&fx.request("nope.swift", &before, &after))).unwrap();
        assert!(abstain.contains("\"abstained\""), "{abstain}");
        assert!(abstain.contains("unsupported_language"), "{abstain}");
    }
}
