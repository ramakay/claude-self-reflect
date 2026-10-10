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
//! [`MAX_COMMITS`]; git is asked for one more than that and stopped there),
//! git fails or outlives its deadline (killed, [`GIT_DEADLINE`]), or an
//! `Unknown` is met first, the caller abstains.
//!
//! Everything is memoized for the lifetime of one [`ReceiptResolver`] (one
//! dream run): the parsed per-file symbol map per `(repo, path, commit)`,
//! whole-file stamps, and the commit list per `(repo, path, from, to)`.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

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
    /// `symbol -> normalized span stamp`; `None` marks a name whose span
    /// cannot be told apart from a neighbour's at this commit (see
    /// [`ambiguous_symbols`]), which reads as [`SymbolState::Unknown`].
    Map(Rc<HashMap<String, Option<String>>>),
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
                Some(Some(stamp)) => SymbolState::Present(stamp.clone()),
                Some(None) => SymbolState::Unknown,
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
    /// `None` when git fails or times out, or the list exceeds [`MAX_COMMITS`].
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
        let result = git_commits_touching(repo_root, relpath, from, to, MAX_COMMITS, GIT_DEADLINE)
            .map(Rc::new);
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

/// `Some(base)` when `symbol` carries the positional collision suffix the
/// HEAD pass appends to duplicate names (`foo#2` -> `foo`).
fn collision_base(symbol: &str) -> Option<&str> {
    let i = symbol.rfind('#')?;
    (i > 0 && i + 1 < symbol.len() && symbol[i + 1..].bytes().all(|b| b.is_ascii_digit()))
        .then(|| &symbol[..i])
}

/// Indices (into `spans`, inclusive `(start, end)` byte offsets of the AST
/// nodes) of every span that overlaps another without one strictly containing
/// the other. Identical ranges overlap. A span nested inside another (a
/// method in its class) is fine: the container's content legitimately
/// includes it.
///
/// Why: two distinct definitions never share bytes unless the AST says one
/// is inside the other, so a partial overlap or an identical range means the
/// extractor named one piece of text twice - a verdict about either would be
/// about both. (Line ranges cannot answer this: `fn foo() {\n 1\n} fn bar()
/// { 2 }` has foo at lines 0-2 and bar at 2-2, which looks nested but is not.)
fn ambiguous_spans(spans: &[(usize, usize)]) -> Vec<bool> {
    let mut order: Vec<usize> = (0..spans.len()).collect();
    // Start ascending, end descending: a container sorts before what it holds.
    order.sort_by(|&a, &b| {
        spans[a]
            .0
            .cmp(&spans[b].0)
            .then(spans[b].1.cmp(&spans[a].1))
    });
    let mut flagged = vec![false; spans.len()];
    for (pos, &i) in order.iter().enumerate() {
        let (start_i, end_i) = spans[i];
        for &j in &order[pos + 1..] {
            let (start_j, end_j) = spans[j];
            if start_j > end_i {
                break;
            }
            let identical = start_j == start_i && end_j == end_i;
            if identical || end_j > end_i {
                flagged[i] = true;
                flagged[j] = true;
            }
        }
    }
    flagged
}

/// Normalized stamp of every symbol span in `bytes`, named exactly as the
/// HEAD pass names them. `None` when the parse is not clean, the language is
/// unsupported, or a span does not fit the file (all of which are Unknown).
///
/// A name whose state cannot be trusted maps to `None` (reads as Unknown):
/// - its AST byte range overlaps a neighbour's without containing it ([`ambiguous_spans`]);
/// - it heads a collision group (`foo` while `foo#2` exists): the bare name
///   goes to the span-earliest duplicate, so a same-named definition added
///   above an unchanged `foo` silently renames the original to `foo#2`.
fn symbol_stamps(
    subject: &Subject,
    commit: &str,
    bytes: &[u8],
) -> Option<HashMap<String, Option<String>>> {
    let derived = rederive_symbol_spans(
        bytes,
        subject.file,
        subject.repo_root,
        subject.project,
        commit,
        &|_| true,
    )?;
    let mut spans = Vec::with_capacity(derived.nodes.len());
    let mut stamps = Vec::with_capacity(derived.nodes.len());
    // Hash the node's EXACT bytes, not whole lines: two definitions sharing
    // a line (`} fn bar() { 2 }`) must not share, or leak edits through,
    // each other's stamp.
    for &(start, end) in &derived.byte_ranges {
        if end <= start || end > bytes.len() {
            return None;
        }
        spans.push((start, end - 1));
        stamps.push(stamp_normalized(&bytes[start..end]).as_str().to_string());
    }
    let overlapping = ambiguous_spans(&spans);
    let mut out: HashMap<String, Option<String>> = HashMap::with_capacity(derived.symbols.len());
    for ((symbol, stamp), overlaps) in derived.symbols.iter().zip(stamps).zip(overlapping) {
        out.insert(symbol.clone(), (!overlaps).then_some(stamp));
    }
    for symbol in &derived.symbols {
        if let Some(base) = collision_base(symbol) {
            if let Some(entry) = out.get_mut(base) {
                *entry = None;
            }
        }
    }
    Some(out)
}

/// Committer time of `commit` (full hex oid) as RFC 3339 in the committer's
/// OWN UTC offset (`2026-09-12T22:57:11-07:00`), read from the object
/// database. `None` when the commit cannot be read. This is when the change
/// HAPPENED, as opposed to when a dream noticed it, and its first ten
/// characters are the date the committer saw (what `git log` shows).
pub(crate) fn commit_time_local(auditor: &Auditor, commit: &str) -> Option<String> {
    let oid = commit.parse::<ObjectId>().ok()?;
    let time = auditor.repo().find_commit(oid).ok()?.time().ok()?;
    format_commit_time(time.seconds, time.offset)
}

/// RFC 3339 rendering of `seconds` since the epoch at `offset_seconds` east
/// of UTC. `None` when either is out of range.
fn format_commit_time(seconds: i64, offset_seconds: i32) -> Option<String> {
    let zone = chrono::FixedOffset::east_opt(offset_seconds)?;
    let when = chrono::DateTime::<chrono::Utc>::from_timestamp(seconds, 0)?.with_timezone(&zone);
    Some(when.to_rfc3339_opts(chrono::SecondsFormat::Secs, false))
}

/// How long one `git log` receipt search may run before it is killed and the
/// search abstains.
const GIT_DEADLINE: Duration = Duration::from_secs(20);

/// `git log --format=%H --topo-order --reverse --ancestry-path from..to --
/// relpath`, bounded: at most `max + 1` commits are requested (`-n` applies
/// BEFORE `--reverse`, so receiving more than `max` proves the range is
/// longer than the cap and the caller must abstain, while `max` or fewer is
/// the complete list), and the process gets `deadline` to finish.
fn git_commits_touching(
    repo_root: &str,
    relpath: &str,
    from: &str,
    to: &str,
    max: usize,
    deadline: Duration,
) -> Option<Vec<String>> {
    let mut cmd = git_at(repo_root);
    cmd.arg("log")
        .arg("--format=%H")
        .arg("--topo-order")
        .arg("--reverse")
        .arg("--ancestry-path")
        .arg("-n")
        .arg((max + 1).to_string())
        .arg(format!("{from}..{to}"))
        .arg("--")
        .arg(relpath);
    let stdout = run_with_deadline(cmd, deadline)?;
    parse_commit_list(&stdout, max)
}

/// The commit list on `stdout` (one hash per line), or `None` when it is not
/// UTF-8 or holds more than `max` commits.
fn parse_commit_list(stdout: &[u8], max: usize) -> Option<Vec<String>> {
    let text = std::str::from_utf8(stdout).ok()?;
    let commits: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    (commits.len() <= max).then_some(commits)
}

/// Run `cmd` and return its stdout if it exits successfully within
/// `deadline`. On timeout the child is killed and reaped. Stdout is drained
/// on its own thread so a full pipe can never block the child (and with it
/// the deadline poll).
fn run_with_deadline(mut cmd: Command, deadline: Duration) -> Option<Vec<u8>> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut pipe = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        pipe.read_to_end(&mut buf).ok().map(|_| buf)
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            // Timed out, or the wait itself failed: do not leave it running.
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    // After a kill the reader is deliberately not joined: a grandchild that
    // inherited the pipe would hold it open and stall the caller past the
    // deadline. The thread ends by itself once the pipe closes.
    let status = status?;
    let output = reader.join().ok().flatten();
    status.success().then_some(output).flatten()
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
    /// The successor witness row names a commit at which the symbol does not
    /// exist (a stale or mis-minted row): a successor that is not there
    /// cannot certify a supersession.
    SuccessorAbsent,
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
    /// `W` was replaced by the witness minted at `successor_oid`; `head` is
    /// the HEAD the join observed, where the symbol must itself still be
    /// readable and unambiguous - a historical successor row never vouches
    /// for a HEAD whose symbol cannot be told apart from a same-named one.
    Superseded {
        successor_oid: &'a str,
        head: &'a str,
    },
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
        Claim::Superseded {
            successor_oid,
            head,
        } => {
            let successor = match resolver.state_at(subject, successor_oid) {
                SymbolState::Unknown => return Err(Abstain::Unparseable),
                SymbolState::Absent => return Err(Abstain::SuccessorAbsent),
                s if is_base(&s) => return Err(Abstain::SameContent),
                s @ SymbolState::Present(_) => s,
            };
            // The same checks at HEAD, whatever commit the successor row
            // names: an ambiguous (collision-group) or unparseable HEAD
            // symbol abstains, one that is gone is not a replacement, and
            // one that differs from the successor is not the successor's
            // content.
            match resolver.state_at(subject, head) {
                SymbolState::Unknown => return Err(Abstain::Unparseable),
                SymbolState::Absent => return Err(Abstain::SuccessorAbsent),
                s if is_base(&s) => return Err(Abstain::SameContent),
                s if s != successor => return Err(Abstain::NoReceipt),
                SymbolState::Present(_) => {}
            }
            // The change is the first commit that HAS the symbol with other
            // content; a commit where it is merely absent is a deletion, not
            // the successor's content.
            resolver
                .first_commit_where(
                    subject,
                    search_from,
                    successor_oid,
                    &|s| matches!(s, SymbolState::Present(p) if *p != base),
                )
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

#[cfg(test)]
mod tests {
    #[test]
    fn commit_time_is_rendered_in_the_committers_own_offset() {
        // 2026-09-13T05:57:11Z is 22:57:11 the previous day at -07:00.
        let utc = chrono::DateTime::parse_from_rfc3339("2026-09-13T05:57:11Z")
            .unwrap()
            .timestamp();
        assert_eq!(
            format_commit_time(utc, -7 * 3600).as_deref(),
            Some("2026-09-12T22:57:11-07:00")
        );
        assert_eq!(
            format_commit_time(utc, 5 * 3600 + 30 * 60).as_deref(),
            Some("2026-09-13T11:27:11+05:30")
        );
        assert_eq!(
            format_commit_time(utc, 0).as_deref(),
            Some("2026-09-13T05:57:11+00:00")
        );
        assert_eq!(format_commit_time(utc, 100 * 3600), None);
    }

    use super::*;

    #[test]
    fn strict_nesting_is_not_an_overlap() {
        // class (0..5) holding two methods (1..2, 3..4) and a sibling fn.
        let spans = [(0, 5), (1, 2), (3, 4), (7, 8)];
        assert_eq!(ambiguous_spans(&spans), [false; 4]);
        // Nested deeper, sharing the container's first line.
        assert_eq!(ambiguous_spans(&[(0, 9), (0, 4), (1, 3)]), [false; 3]);
    }

    #[test]
    fn identical_and_partially_overlapping_spans_are_ambiguous() {
        // Same line; shared boundary line; and a clean third span.
        let spans = [(3, 3), (3, 3), (10, 12), (12, 14), (20, 21)];
        assert_eq!(ambiguous_spans(&spans), [true, true, true, true, false]);
        // A span straddling the end of a container flags both, not the one
        // it fully contains.
        assert_eq!(
            ambiguous_spans(&[(0, 5), (2, 3), (4, 8)]),
            [true, false, true]
        );
    }

    #[test]
    fn a_commit_list_longer_than_the_cap_abstains() {
        let out = b"a1\nb2\nc3\n";
        assert_eq!(parse_commit_list(out, 3).unwrap(), ["a1", "b2", "c3"]);
        assert!(
            parse_commit_list(out, 2).is_none(),
            "cap + 1 means unbounded"
        );
        assert!(parse_commit_list(b"", 0).unwrap().is_empty());
        assert!(parse_commit_list(&[0xff, 0xfe], 5).is_none());
    }

    fn git_ok(dir: &Path, args: &[&str]) {
        let mut cmd = git_at(&dir.to_string_lossy());
        let out = cmd
            .args(["-c", "user.email=t@example.com", "-c", "user.name=t"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    #[test]
    fn git_search_requests_one_commit_past_the_cap_and_abstains_on_overflow() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git_ok(dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("a.rs"), "// base\n").unwrap();
        git_ok(dir, &["add", "-A"]);
        git_ok(dir, &["commit", "-q", "-m", "base"]);
        let base = {
            let o = git_at(&dir.to_string_lossy())
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap();
            String::from_utf8(o.stdout).unwrap().trim().to_string()
        };
        for n in 0..3 {
            std::fs::write(dir.join("a.rs"), format!("// edit {n}\n")).unwrap();
            git_ok(dir, &["add", "-A"]);
            git_ok(dir, &["commit", "-q", "-m", &format!("edit {n}")]);
        }
        let root = dir.to_string_lossy().to_string();
        let ask = |max| git_commits_touching(&root, "a.rs", &base, "HEAD", max, GIT_DEADLINE);

        let all = ask(3).expect("3 commits fit a cap of 3");
        assert_eq!(all.len(), 3);
        assert!(ask(2).is_none(), "3 commits exceed a cap of 2: abstain");
        assert!(ask(10).is_some());
        // `-n` before `--reverse`: an in-cap list is still oldest first.
        assert_eq!(
            git_commits_touching(&root, "a.rs", &base, "HEAD", 3, GIT_DEADLINE).unwrap(),
            all
        );
    }

    #[test]
    fn a_git_process_past_its_deadline_is_killed_and_the_search_abstains() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "exec sleep 30"]);
        let started = Instant::now();
        assert!(run_with_deadline(cmd, Duration::from_millis(200)).is_none());
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "must not wait out the child: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn output_larger_than_a_pipe_buffer_does_not_deadlock_the_deadline_poll() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "head -c 1000000 /dev/zero"]);
        let out = run_with_deadline(cmd, Duration::from_secs(20)).expect("completes");
        assert_eq!(out.len(), 1_000_000);

        let mut failing = Command::new("sh");
        failing.args(["-c", "echo partial; exit 3"]);
        assert!(run_with_deadline(failing, Duration::from_secs(20)).is_none());
    }
}
