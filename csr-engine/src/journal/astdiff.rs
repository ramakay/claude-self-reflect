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
#[path = "astdiff_tests.rs"]
mod tests;
