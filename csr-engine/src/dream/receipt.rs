//! True receipts for dream conclusions: the commit that actually made the
//! change, found from git history and the committed bytes alone.
//!
//! The join (`super::dream_join_cancellable`) decides a verdict KIND from
//! ledger stamps. Two ledger stamps are weak evidence of change: they come
//! from different span methods (legacy `code_nodes` line spans vs spans
//! re-derived from the blob's AST) and they are raw, so a pure reformat
//! counts as a rewrite. So before a NEW event is written, this module
//! re-reads the symbol itself at the commits that matter and answers with a
//! [`SymbolState`]:
//!
//! - `Present(stamp)`: the symbol's span exists at that commit; `stamp` is
//!   the whitespace-normalized hash of exactly that span (or of the whole
//!   file for a whole-file witness).
//! - `Absent`: the file is not in the commit's tree, or the file parsed
//!   cleanly and the symbol is not in it.
//! - `Unknown` (abstains as `Unparseable`): the blob could not be read or the parse was not clean /
//!   the language is unsupported. Unknown is NEVER read as absent or as
//!   changed: every search that meets one before a match abstains.
//!
//! Symbol names come from `import::backfill::rederive_symbol_spans`, the
//! same function the HEAD stamp-spans pass uses, so a ledger symbol and the
//! name looked up here agree by construction.
//!
//! A receipt is the FIRST commit, oldest first along
//! `git log --ancestry-path --reverse <from>..<to> -- <path>`, whose state
//! satisfies the predicate. There is no fallback receipt: no HEAD, no
//! successor `at_oid`. If the search is unbounded (more than
//! [`MAX_COMMITS`]), git fails, or an `Unknown` is met first, the caller
//! abstains.
//!
//! Everything is memoized for the lifetime of one [`ReceiptResolver`] (one
//! dream run): the parsed per-file symbol map per `(repo, path, commit)`,
//! whole-file stamps, and the commit list per `(repo, path, from, to)`.

use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use codewitness::{stamp_normalized, Auditor, ObjectId};

use crate::import::backfill::{git_at, rederive_symbol_spans};

/// Upper bound on the commits one receipt search will walk. A longer span
/// is not searched at all (abstain): an unbounded scan of a hot file's
/// history is both slow and a sign the pair of endpoints is not a tight
/// claim.
pub(crate) const MAX_COMMITS: usize = 5_000;

/// Cap on memoized per-file symbol maps before the memo is dropped. Anchors
/// arrive grouped by file, so entries for earlier files are never reused.
const MAX_MEMOIZED_FILES: usize = 2_048;

/// The state of one symbol (or whole file) at one commit. See the module doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SymbolState {
    Present(String),
    Absent,
    Unknown,
}

/// What a state lookup is about. Borrowed bundle so the helpers do not
/// take eight arguments.
pub(crate) struct Subject<'a> {
    pub auditor: &'a Auditor,
    pub repo_root: &'a str,
    /// Absolute path as stored on the ledger row (feeds language detection
    /// and node ids in the shared extractor).
    pub file: &'a str,
    /// Path relative to `repo_root`.
    pub relpath: &'a str,
    pub project: &'a str,
    /// `None` = whole-file witness.
    pub symbol: Option<&'a str>,
}

type FileKey = (String, String, String);
type CommitsKey = (String, String, String, String);

/// File read at one commit.
#[derive(Clone)]
enum FileRead {
    Absent,
    Unknown,
    Bytes(Rc<Vec<u8>>),
}

/// Parsed symbol map for one file at one commit.
#[derive(Clone)]
enum SymbolMap {
    /// File not in the commit's tree.
    FileAbsent,
    /// Unreadable, unsupported language, or not a clean parse.
    Unknown,
    Map(Rc<HashMap<String, String>>),
}

/// Per-run memo + counters.
#[derive(Default)]
pub(crate) struct ReceiptResolver {
    /// Normalized stamp of the whole file (or Absent/Unknown) per commit.
    whole: HashMap<FileKey, SymbolState>,
    /// Parsed `symbol -> normalized span stamp` map per commit.
    symbols: HashMap<FileKey, SymbolMap>,
    commits: HashMap<CommitsKey, Option<Rc<Vec<String>>>>,
    /// `git log` invocations actually run (memo misses). Observable proof a
    /// run did or did not search.
    pub searches: usize,
    /// Blob parses actually run (memo misses).
    pub parses: usize,
}

impl ReceiptResolver {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn read(&self, subject: &Subject, commit: &str) -> FileRead {
        let Ok(oid) = commit.parse::<ObjectId>() else {
            return FileRead::Unknown;
        };
        match subject
            .auditor
            .file_content_at(Path::new(subject.relpath), oid)
        {
            Ok(bytes) => FileRead::Bytes(Rc::new(bytes)),
            Err(codewitness::Error::AnchorMissing { .. }) => FileRead::Absent,
            Err(_) => FileRead::Unknown,
        }
    }

    /// State of `subject` at `commit` (full hex oid).
    pub(crate) fn state_at(&mut self, subject: &Subject, commit: &str) -> SymbolState {
        let key: FileKey = (
            subject.repo_root.to_string(),
            subject.relpath.to_string(),
            commit.to_string(),
        );
        let Some(symbol) = subject.symbol else {
            if let Some(hit) = self.whole.get(&key) {
                return hit.clone();
            }
            let state = match self.read(subject, commit) {
                FileRead::Absent => SymbolState::Absent,
                FileRead::Unknown => SymbolState::Unknown,
                FileRead::Bytes(bytes) => {
                    SymbolState::Present(stamp_normalized(&bytes).as_str().to_string())
                }
            };
            self.whole.insert(key, state.clone());
            return state;
        };

        match self.symbol_map(subject, commit) {
            SymbolMap::FileAbsent => SymbolState::Absent,
            SymbolMap::Map(map) => match map.get(symbol) {
                Some(stamp) => SymbolState::Present(stamp.clone()),
                None => SymbolState::Absent,
            },
            SymbolMap::Unknown => SymbolState::Unknown,
        }
    }

    /// Parsed symbol map of the subject's file at `commit`, memoized.
    fn symbol_map(&mut self, subject: &Subject, commit: &str) -> SymbolMap {
        let key: FileKey = (
            subject.repo_root.to_string(),
            subject.relpath.to_string(),
            commit.to_string(),
        );
        if let Some(hit) = self.symbols.get(&key) {
            return hit.clone();
        }
        if self.symbols.len() >= MAX_MEMOIZED_FILES {
            self.symbols.clear();
        }
        let parsed = match self.read(subject, commit) {
            FileRead::Absent => SymbolMap::FileAbsent,
            FileRead::Unknown => SymbolMap::Unknown,
            FileRead::Bytes(bytes) => {
                self.parses += 1;
                match symbol_stamps(subject, commit, &bytes) {
                    Some(map) => SymbolMap::Map(Rc::new(map)),
                    None => SymbolMap::Unknown,
                }
            }
        };
        self.symbols.insert(key, parsed.clone());
        parsed
    }

    /// Whether the file at `commit` still has a symbol whose leaf name
    /// equals the subject symbol's leaf, under ANY qualification (see
    /// [`symbol_leaf`]). `Some(false)` when the file is not in the commit's
    /// tree; `None` when the file's symbols cannot be established (Unknown)
    /// or the subject is a whole-file witness.
    pub(crate) fn leaf_present_at(&mut self, subject: &Subject, commit: &str) -> Option<bool> {
        let leaf = symbol_leaf(subject.symbol?);
        match self.symbol_map(subject, commit) {
            SymbolMap::FileAbsent => Some(false),
            SymbolMap::Map(map) => Some(map.keys().any(|name| symbol_leaf(name) == leaf)),
            SymbolMap::Unknown => None,
        }
    }

    /// Commits that touched `relpath` strictly after `from` up to and
    /// including `to`, oldest first, restricted to `to`'s ancestry path.
    /// `None` when git fails or the list exceeds [`MAX_COMMITS`].
    pub(crate) fn commits_touching(
        &mut self,
        repo_root: &str,
        relpath: &str,
        from: &str,
        to: &str,
    ) -> Option<Rc<Vec<String>>> {
        let key: CommitsKey = (
            repo_root.to_string(),
            relpath.to_string(),
            from.to_string(),
            to.to_string(),
        );
        if let Some(hit) = self.commits.get(&key) {
            return hit.clone();
        }
        self.searches += 1;
        let result = git_commits_touching(repo_root, relpath, from, to).map(Rc::new);
        self.commits.insert(key, result.clone());
        result
    }

    /// First commit (oldest first) in `from..to` touching the file whose
    /// state satisfies `pred`. An `Unknown` state met before a match
    /// abstains (`None`), as does an unbounded or failed history walk.
    pub(crate) fn first_commit_where(
        &mut self,
        subject: &Subject,
        from: &str,
        to: &str,
        pred: &dyn Fn(&SymbolState) -> bool,
    ) -> Option<String> {
        let commits = self.commits_touching(subject.repo_root, subject.relpath, from, to)?;
        for commit in commits.iter() {
            let state = self.state_at(subject, commit);
            if state == SymbolState::Unknown {
                return None;
            }
            if pred(&state) {
                return Some(commit.clone());
            }
        }
        None
    }
}

/// Leaf name of a ledger symbol: the `#N` collision suffix stripped, then
/// everything after the last `.` or `::` container separator
/// (`make_renderer.render` -> `render`, `Foo::bar#2` -> `bar`).
pub(crate) fn symbol_leaf(symbol: &str) -> &str {
    let base = match symbol.rfind('#') {
        Some(i) if i + 1 < symbol.len() && symbol[i + 1..].bytes().all(|b| b.is_ascii_digit()) => {
            &symbol[..i]
        }
        _ => symbol,
    };
    let after_dot = base.rfind('.').map(|i| i + 1);
    let after_colons = base.rfind("::").map(|i| i + 2);
    match after_dot.max(after_colons) {
        Some(i) => &base[i..],
        None => base,
    }
}

/// Normalized stamp of every symbol span in `bytes`, named exactly as the
/// HEAD pass names them. `None` when the parse is not clean, the language is
/// unsupported, or a span does not fit the file (all of which are Unknown).
fn symbol_stamps(subject: &Subject, commit: &str, bytes: &[u8]) -> Option<HashMap<String, String>> {
    let derived = rederive_symbol_spans(
        bytes,
        subject.file,
        subject.repo_root,
        subject.project,
        commit,
        &|_| true,
    )?;
    let lines: Vec<&[u8]> = {
        let mut lines: Vec<&[u8]> = if bytes.is_empty() {
            Vec::new()
        } else {
            bytes.split(|&b| b == b'\n').collect()
        };
        if bytes.ends_with(b"\n") {
            lines.pop();
        }
        lines
    };
    let mut out = HashMap::with_capacity(derived.nodes.len());
    for (node, symbol) in derived.nodes.iter().zip(derived.symbols) {
        // 0-based row span -> 1-based inclusive lines, the convention the
        // HEAD pass hands to `Anchor::with_span`.
        let (start, end) = (node.span_start, node.span_end);
        if start < 0 || end < start {
            return None;
        }
        let (start, end) = (usize::try_from(start).ok()?, usize::try_from(end).ok()?);
        if end >= lines.len() {
            return None;
        }
        let mut span = Vec::new();
        for line in &lines[start..=end] {
            span.extend_from_slice(line);
            span.push(b'\n');
        }
        out.insert(symbol, stamp_normalized(&span).as_str().to_string());
    }
    Some(out)
}

/// `git log --format=%H --topo-order --reverse --ancestry-path from..to --
/// relpath`. Never `-n` (it applies before `--reverse`).
fn git_commits_touching(
    repo_root: &str,
    relpath: &str,
    from: &str,
    to: &str,
) -> Option<Vec<String>> {
    let output = git_at(repo_root)
        .arg("log")
        .arg("--format=%H")
        .arg("--topo-order")
        .arg("--reverse")
        .arg("--ancestry-path")
        .arg(format!("{from}..{to}"))
        .arg("--")
        .arg(relpath)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let commits: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    (commits.len() <= MAX_COMMITS).then_some(commits)
}

/// Why a candidate event was withheld instead of written with a receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Abstain {
    /// A needed commit's content could not be established: the file's parse
    /// there is not clean (or the language is unsupported), or the blob is
    /// unreadable / its path could not be located. Never read as absent.
    Unparseable,
    /// The parse at the witness's own commit is clean but the witness symbol
    /// is not in the file (or the file is not in that commit's tree) under
    /// the shared naming: the ledger row claims a symbol at a commit that
    /// never contained it (typical of legacy `source_kind='backfill'` rows).
    /// Also covers a reinstatement whose symbol, re-read at HEAD, does not
    /// have the witness's content.
    NotAtWitness,
    /// The join found no stamp at HEAD, but the symbol is in the HEAD blob.
    PresentAtHead,
    /// The successor's content equals the witness's (reformat / span-method
    /// artifact): nothing was superseded.
    SameContent,
    /// The qualified symbol is gone but the file still has a symbol with the
    /// same leaf name (moved out of / into another container, or
    /// re-qualified), either at the receipt commit or at HEAD: a rename of
    /// the qualification, not a deletion.
    Moved,
    /// The conclusion holds but no commit in the range can be named as the
    /// one that made the change (empty/oversized range, git failure, or an
    /// unknown state met first).
    NoReceipt,
}

/// What the join concluded, with the commits the receipt rules need.
pub(crate) enum Claim<'a> {
    /// `W` went stale: gone at `head`.
    Obsolete { head: &'a str },
    /// `W` was replaced by the witness minted at `successor_oid`.
    Superseded { successor_oid: &'a str },
    /// `W` is back at `head`; the negative verdict it carried was observed
    /// at `negative_observed_head`.
    Reinstated {
        head: &'a str,
        negative_observed_head: &'a str,
    },
}

/// Resolve the commit that made the change behind `claim` for the witness
/// minted at `w_oid`, or the reason to abstain. See the module doc.
///
/// `boundary` is the HEAD at which the witness was last reinstated, if its
/// latest live event is a reinstatement. A new negative conclusion
/// (obsolete / superseded) searches from there instead of from `w_oid`:
/// commits before it belong to a change the reinstatement cancelled.
pub(crate) fn resolve_receipt(
    resolver: &mut ReceiptResolver,
    subject: &Subject,
    w_oid: &str,
    boundary: Option<&str>,
    claim: &Claim,
) -> Result<String, Abstain> {
    let search_from = boundary.unwrap_or(w_oid);
    let base = match resolver.state_at(subject, w_oid) {
        SymbolState::Present(base) => base,
        SymbolState::Absent => return Err(Abstain::NotAtWitness),
        SymbolState::Unknown => return Err(Abstain::Unparseable),
    };
    let is_base = |s: &SymbolState| matches!(s, SymbolState::Present(p) if *p == base);
    match claim {
        Claim::Obsolete { head } => {
            match resolver.state_at(subject, head) {
                SymbolState::Absent => {}
                SymbolState::Present(_) => return Err(Abstain::PresentAtHead),
                SymbolState::Unknown => return Err(Abstain::Unparseable),
            }
            // Moved, not deleted: a same-leaf symbol still exists at HEAD
            // under some other qualification.
            if resolver.leaf_present_at(subject, head) == Some(true) {
                return Err(Abstain::Moved);
            }
            let receipt = resolver
                .first_commit_where(subject, search_from, head, &|s| *s == SymbolState::Absent)
                .ok_or(Abstain::NoReceipt)?;
            // ... or already at the commit that dropped the qualified name.
            if resolver.leaf_present_at(subject, &receipt) == Some(true) {
                return Err(Abstain::Moved);
            }
            Ok(receipt)
        }
        Claim::Superseded { successor_oid } => {
            match resolver.state_at(subject, successor_oid) {
                SymbolState::Unknown => return Err(Abstain::Unparseable),
                s if is_base(&s) => return Err(Abstain::SameContent),
                _ => {}
            }
            resolver
                .first_commit_where(subject, search_from, successor_oid, &|s| !is_base(s))
                .ok_or(Abstain::NoReceipt)
        }
        Claim::Reinstated {
            head,
            negative_observed_head,
        } => {
            match resolver.state_at(subject, head) {
                SymbolState::Unknown => return Err(Abstain::Unparseable),
                s if !is_base(&s) => return Err(Abstain::NotAtWitness),
                _ => {}
            }
            resolver
                .first_commit_where(subject, negative_observed_head, head, &is_base)
                .ok_or(Abstain::NoReceipt)
        }
    }
}
