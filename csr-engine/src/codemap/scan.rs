//! Scanner: turns a repo's worktrees into rows.
//!
//! `scan_repo` maps every view of a repo; `rescan_paths` redoes a handful of
//! paths in the views that hold them; `view_changed` is the cheap probe that
//! says whether a full `scan_repo` is worth running.
//!
//! What a view holds: every tracked regular file with a code extension (taken
//! from the index, in path order), plus untracked code files that survive
//! `.gitignore` (a directory with its own `.git` is never entered: linked
//! worktrees live inside their main worktree). At most
//! `MAX_FILES_PER_PROJECT` per view. Hidden (dot) directories are walked for
//! untracked files, so untracked planning dirs like `.plans/` are mapped; a few
//! tool-cache dot directories (`UNTRACKED_SKIP_DIRS`) are not.
//!
//! Dirty classification is index-relative and needs no git command: a file whose
//! (mtime, size) equals its index entry's, and whose mtime predates the index
//! file's, is clean. Anything else is hashed as a git blob and compared with the
//! index oid. Content that a clean/smudge filter (autocrlf, LFS) rewrites can
//! therefore read as modified when its stat differs; the blob is still the bytes
//! on disk.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use ignore::gitignore::GitignoreBuilder;
use ignore::WalkBuilder;
use rusqlite::Connection;
use serde::Serialize;

use super::identity::{
    self, git_blob_oid, head_of, index_stat, is_tcc_path, normalize_lexical, IndexEntry, LiveIndex,
    ViewId,
};
use super::store::{self, DefRow, FileRec, ParsedBlob, StoredView, ViewWrite};
use crate::extraction::ast_analysis::lang_from_path_str;
use crate::extraction::codegraph::extract_graph_fragment_for_file;
use crate::extraction::repo_scan::{
    has_code_extension, is_hard_skip_dir, looks_minified, MAX_FILES_PER_PROJECT, MAX_FILE_BYTES,
};

/// Dot directories that hold tool output, not source, when nothing ignores them.
/// Applied to untracked files only: a tracked file under one was committed on
/// purpose.
const UNTRACKED_SKIP_DIRS: &[&str] = &[
    ".venv",
    ".tox",
    ".cache",
    ".mypy_cache",
    ".pytest_cache",
    ".idea",
];

/// Longest `signature` kept per def.
const MAX_SIGNATURE_CHARS: usize = 200;

#[derive(Debug, Clone)]
pub struct ScanOpts {
    /// Per-view file cap. Production uses `MAX_FILES_PER_PROJECT`; tests lower it.
    pub max_files_per_view: usize,
    /// Home directory for the protected-folder check; `$HOME` when `None`.
    pub home: Option<PathBuf>,
}

impl Default for ScanOpts {
    fn default() -> Self {
        Self {
            max_files_per_view: MAX_FILES_PER_PROJECT,
            home: None,
        }
    }
}

impl ScanOpts {
    fn home(&self) -> Option<PathBuf> {
        self.home.clone().or_else(identity::home_dir)
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ViewReport {
    pub path: String,
    pub branch: Option<String>,
    /// First 12 hex chars of HEAD.
    pub head: Option<String>,
    pub files: usize,
    /// Files modified in the worktree (differ from the index).
    pub dirty: usize,
    /// Untracked code files.
    pub untracked: usize,
    pub truncated: bool,
    pub skipped_large: usize,
    pub ms: u64,
    /// `ok`, or `error: <why>` when this view failed and was left as it was.
    pub status: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ScanTotals {
    /// (view, path) pairs across all views.
    pub file_pairs: usize,
    /// Distinct blob oids seen in this scan.
    pub unique_blobs_seen: usize,
    pub blobs_parsed: usize,
    /// Distinct blobs that were already stored and needed no parse.
    pub blobs_reused: usize,
    pub defs_written: usize,
    pub gc_blobs: usize,
    pub ms: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RepoScanReport {
    pub common_dir: String,
    pub views: Vec<ViewReport>,
    /// Registered worktrees that no longer exist (prunable) or were refused.
    pub stale_worktrees: usize,
    /// Stored views dropped because their worktree is gone.
    pub removed_views: usize,
    pub totals: ScanTotals,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PathScanReport {
    pub paths: usize,
    pub updated: usize,
    pub removed: usize,
    pub unchanged: usize,
    pub skipped_protected: usize,
    pub skipped_outside_views: usize,
    pub skipped_caps: usize,
    pub skipped_ignored: usize,
    pub blobs_parsed: usize,
    pub defs_written: usize,
    pub gc_blobs: usize,
    pub views_touched: Vec<String>,
    /// Repos (common dirs) whose paths were left alone because another scanner
    /// held the repo lock for the whole wait.
    pub locked_repos: Vec<String>,
    pub ms: u64,
}

/// Test-only seam: a closure run on the scanning thread between reading a
/// view's files and writing them, so a test can interleave an edit and a
/// partial rescan at exactly that point. Thread-local, so parallel tests that
/// scan do not see each other's hook.
#[cfg(test)]
pub(super) mod test_hooks {
    use std::cell::{Cell, RefCell};

    type Hook = Box<dyn Fn()>;

    thread_local! {
        static AFTER_COLLECT: RefCell<Option<Hook>> = const { RefCell::new(None) };
        // (files returned, entries visited, stopped) of the last full-scan
        // untracked walk on this thread.
        static LAST_WALK: Cell<Option<(usize, usize, bool)>> = const { Cell::new(None) };
        // Paths `expand_dir_events` asked `lstat` about, in order.
        static LSTATS: RefCell<Vec<std::path::PathBuf>> = const { RefCell::new(Vec::new()) };
    }

    pub(super) fn record_lstat(path: &std::path::Path) {
        LSTATS.with(|l| l.borrow_mut().push(path.to_path_buf()));
    }

    pub(crate) fn take_lstats() -> Vec<std::path::PathBuf> {
        LSTATS.with(|l| std::mem::take(&mut *l.borrow_mut()))
    }

    pub(super) fn record_walk(files: usize, visited: usize, stopped: bool) {
        LAST_WALK.with(|w| w.set(Some((files, visited, stopped))));
    }

    pub(crate) fn last_walk() -> Option<(usize, usize, bool)> {
        LAST_WALK.with(Cell::get)
    }

    pub(crate) fn set_after_collect(hook: impl Fn() + 'static) {
        AFTER_COLLECT.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
    }

    pub(super) fn after_collect() {
        AFTER_COLLECT.with(|h| {
            if let Some(f) = h.borrow().as_ref() {
                f();
            }
        });
    }
}

// ---- caps and file facts ----------------------------------------------------

/// True when a repo-relative '/'-separated path is a scannable code file: a code
/// extension and no hard-skip directory segment.
pub(crate) fn rel_passes_caps(rel: &str) -> bool {
    if !has_code_extension(Path::new(rel)) {
        return false;
    }
    let mut parts: Vec<&str> = rel.split('/').collect();
    parts.pop();
    parts.iter().all(|d| !is_hard_skip_dir(d))
}

pub(super) struct FileStat {
    size: u64,
    mtime_secs: u32,
    mtime_nsecs: u32,
}

impl FileStat {
    fn mtime_ns(&self) -> i64 {
        self.mtime_secs as i64 * 1_000_000_000 + self.mtime_nsecs as i64
    }
}

#[cfg(test)]
pub(super) fn test_stat(secs: u32, nsecs: u32, size: u64) -> FileStat {
    FileStat {
        size,
        mtime_secs: secs,
        mtime_nsecs: nsecs,
    }
}

/// Stat of a regular file (symlinks and directories are `None`).
fn stat_regular(abs: &Path) -> Option<FileStat> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(abs).ok()?;
    if !meta.is_file() {
        return None;
    }
    Some(FileStat {
        size: meta.len(),
        mtime_secs: meta.mtime() as u32,
        mtime_nsecs: meta.mtime_nsec() as u32,
    })
}

/// True when the index entry proves the file unchanged without reading it.
/// Entries written in the same second as the index file are "racily clean" and
/// must be hashed.
pub(super) fn clean_by_stat(
    entry: &IndexEntry,
    st: &FileStat,
    index_mtime: Option<(u32, u32)>,
) -> bool {
    let Some((idx_secs, _)) = index_mtime else {
        return false;
    };
    entry.mtime_secs < idx_secs
        && entry.mtime_secs == st.mtime_secs
        && (entry.mtime_nsecs == st.mtime_nsecs || entry.mtime_nsecs == 0)
        && entry.size == st.size as u32
}

// ---- parsing ------------------------------------------------------------------

fn first_line_signature(lines: &[&str], span_start: i64) -> String {
    let line = usize::try_from(span_start)
        .ok()
        .and_then(|i| lines.get(i))
        .copied()
        .unwrap_or("");
    line.trim().chars().take(MAX_SIGNATURE_CHARS).collect()
}

/// `x.min.js`: skipped by name, whatever the content.
fn has_min_js_name(rel: &str) -> bool {
    Path::new(rel)
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(".min.js"))
}

/// The content half of `looks_minified`: a line longer than the extractor's
/// threshold. Asked with a neutral file name so the name rule cannot fire.
fn looks_minified_by_content(source: &str) -> bool {
    looks_minified(Path::new("content.txt"), source)
}

/// A stored status as a static name; anything unknown reads as final (`error`).
fn status_name(s: &str) -> &'static str {
    match s {
        "ok" => "ok",
        "no_lang" => "no_lang",
        "minified_name" => "minified_name",
        "minified" => "minified",
        _ => "error",
    }
}

/// Can this path parse a blob that a different path could not? It has a parser
/// and is not a `.min.js` name.
fn path_can_parse(rel: &str) -> bool {
    lang_from_path_str(rel).is_some() && !has_min_js_name(rel)
}

/// Parse one blob into definition rows. Kept kinds are everything the code-graph
/// extractor emits as a symbol except the synthetic `module` anchor and
/// `import`: `function`, `type` and `const`.
pub(crate) fn parse_blob(oid: &str, rel: &str, bytes: &[u8], common_dir: &str) -> ParsedBlob {
    let blob = |status: &'static str, lang: Option<String>, defs: Vec<DefRow>| ParsedBlob {
        oid: oid.to_string(),
        lang,
        status,
        defs,
    };
    let Ok(source) = std::str::from_utf8(bytes) else {
        return blob("error", None, Vec::new());
    };
    // Statuses `no_lang` and `minified_name` depend on the PATH this blob was
    // first seen under, and a blob is stored once whatever paths hold it, so
    // `BlobParser::is_known` lets a path that could do better reparse them.
    // `minified` depends on the content alone and is final.
    if lang_from_path_str(rel).is_none() {
        return blob("no_lang", None, Vec::new());
    }
    if has_min_js_name(rel) {
        return blob("minified_name", None, Vec::new());
    }
    if looks_minified_by_content(source) {
        return blob("minified", None, Vec::new());
    }
    let fragment = extract_graph_fragment_for_file(source, rel, common_dir, "", "", "");
    let lang = fragment
        .nodes
        .iter()
        .find(|n| n.kind == "module")
        .map(|n| n.lang.clone());
    let lines: Vec<&str> = source.lines().collect();
    let mut seen: BTreeSet<(String, String, i64, i64)> = BTreeSet::new();
    let mut defs = Vec::new();
    for n in fragment
        .nodes
        .iter()
        .filter(|n| n.kind != "module" && n.kind != "import")
    {
        if !seen.insert((n.kind.clone(), n.name.clone(), n.span_start, n.span_end)) {
            continue;
        }
        // Innermost lexical container that is not the def itself.
        let container = fragment
            .containers
            .iter()
            .filter(|c| {
                c.start <= n.span_start
                    && n.span_end <= c.end
                    && !(c.start == n.span_start && c.end == n.span_end)
            })
            .min_by_key(|c| c.end - c.start);
        defs.push(DefRow {
            name: n.name.clone(),
            fqname: container.map(|c| format!("{}{}{}", c.name, c.separator, n.name)),
            kind: n.kind.clone(),
            span_start: n.span_start,
            span_end: n.span_end,
            signature: first_line_signature(&lines, n.span_start),
        });
    }
    blob("ok", lang, defs)
}

/// Decides which blobs need parsing and holds the results until a view's
/// transaction writes them.
struct BlobParser<'a> {
    conn: &'a Connection,
    common_dir: &'a str,
    /// Blob oids known stored (current extractor version) or parsed here, with
    /// the status they hold.
    known: HashMap<String, &'static str>,
    parsed: Vec<ParsedBlob>,
}

impl<'a> BlobParser<'a> {
    fn new(conn: &'a Connection, common_dir: &'a str) -> Self {
        Self {
            conn,
            common_dir,
            known: HashMap::new(),
            parsed: Vec::new(),
        }
    }

    /// Does this blob need no parse for `rel`? A stored `ok`, `error` or
    /// `minified` row is final. A stored `no_lang` or `minified_name` row only
    /// says the path it was first seen under could not parse it: a path that
    /// can (`path_can_parse`) parses it again, and the result replaces the row
    /// only when it is `ok`.
    fn is_known(&mut self, oid: &str, rel: &str) -> Result<bool> {
        let status = match self.known.get(oid) {
            Some(s) => *s,
            None => match store::blob_status(self.conn, oid)? {
                Some(s) => {
                    let s = status_name(&s);
                    self.known.insert(oid.to_string(), s);
                    s
                }
                None => return Ok(false),
            },
        };
        Ok(match status {
            "no_lang" | "minified_name" => !path_can_parse(rel),
            _ => true,
        })
    }

    fn parse(&mut self, oid: &str, rel: &str, bytes: &[u8]) {
        let blob = parse_blob(oid, rel, bytes, self.common_dir);
        // Never downgrade what is known: a path-dependent negative must not
        // hide an `ok` this session already parsed.
        let keep = self.known.get(oid).copied();
        if keep != Some("ok") {
            self.known.insert(oid.to_string(), blob.status);
        }
        self.parsed.push(blob);
    }

    /// Hand over what was parsed since the last call (the caller writes it).
    fn take(&mut self) -> Vec<ParsedBlob> {
        std::mem::take(&mut self.parsed)
    }
}

/// Counters shared by the views of one `scan_repo` call.
#[derive(Default)]
struct ScanCounters {
    seen: HashSet<String>,
    parsed: usize,
    reused: usize,
}

// ---- one view ---------------------------------------------------------------------

struct ViewScan {
    files: Vec<FileRec>,
    truncated: bool,
    skipped_large: usize,
}

fn read_capped(abs: &Path) -> Option<Vec<u8>> {
    let bytes = std::fs::read(abs).ok()?;
    (bytes.len() as u64 <= MAX_FILE_BYTES).then_some(bytes)
}

/// Settle one file: its blob oid, dirty flag and (when its blob is unknown) the
/// parse. `index_oid` is `None` for untracked files. `bytes` are the file's
/// content if the caller already read it.
#[allow(clippy::too_many_arguments)]
fn settle_file(
    parser: &mut BlobParser<'_>,
    counters: &mut ScanCounters,
    rel: &str,
    abs: &Path,
    st: &FileStat,
    index_oid: Option<&str>,
    stat_clean: bool,
    stats_sink: &mut Vec<FileRec>,
) -> Result<()> {
    let mut bytes: Option<Vec<u8>> = None;
    let (mut oid, mut dirty) = match (index_oid, stat_clean) {
        (Some(i), true) => (i.to_string(), 0),
        _ => {
            let Some(b) = read_capped(abs) else {
                return Ok(());
            };
            let h = git_blob_oid(&b);
            bytes = Some(b);
            match index_oid {
                Some(i) if i == h => (h, 0),
                Some(_) => (h, 1),
                None => (h, 2),
            }
        }
    };
    let first_sight = counters.seen.insert(oid.clone());
    if parser.is_known(&oid, rel)? {
        if first_sight {
            counters.reused += 1;
        }
    } else {
        // Unknown blob: parse the bytes on disk. A stat-clean file is read now;
        // if it changed since the stat, what is on disk wins.
        let b = match bytes.take() {
            Some(b) => b,
            None => match read_capped(abs) {
                Some(b) => b,
                None => return Ok(()),
            },
        };
        let h = git_blob_oid(&b);
        if h != oid {
            dirty = match index_oid {
                Some(i) if i == h => 0,
                Some(_) => 1,
                None => 2,
            };
            oid = h;
            counters.seen.insert(oid.clone());
        }
        if !parser.is_known(&oid, rel)? {
            parser.parse(&oid, rel, &b);
            counters.parsed += 1;
        }
    }
    stats_sink.push(FileRec {
        path: rel.to_string(),
        blob_oid: oid,
        dirty,
        size: st.size as i64,
        mtime_ns: st.mtime_ns(),
    });
    Ok(())
}

/// Entries (files and directories, ignored-by-name ones excluded) a walk may
/// visit, as a multiple of the file cap. A tree full of non-code files or empty
/// directories never reaches the file cap, so the cap alone does not bound it.
const WALK_VISIT_FACTOR: usize = 20;

/// The visit budget for a walk whose file cap is `cap`.
pub(super) fn walk_visit_budget(cap: usize) -> usize {
    cap.saturating_mul(WALK_VISIT_FACTOR)
}

/// What a bounded walk found.
pub(super) struct WalkOutcome {
    /// Repo-relative '/'-separated paths of code files, in path order.
    pub(super) files: Vec<String>,
    /// The walk stopped before the end: more files than the allowance, or more
    /// entries than the visit budget.
    pub(super) stopped: bool,
    /// Entries the walk visited (the root excluded), at most the visit budget.
    /// Read by the tests that prove the budget holds.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) visited: usize,
}

/// The walker the full scan's untracked pass and the directory-event pass share:
/// `.gitignore`, `.git/info/exclude` apply; hard-skip directories, tool-cache
/// dot directories and any directory with its own `.git` are not entered.
/// `only_under` restricts the walk to that directory (plus the ancestors that
/// lead to it, so their `.gitignore` files still apply).
///
/// The `ignore` crate opens ignore files with plain opens, which follow a
/// symlink wherever it leads, so no ignore file is left to the crate unless it
/// has been checked (`ignore_file_is_protected_link`):
///
/// - `.git/info/exclude` and the global excludes are added here, after
///   `resolve_safe` (`git_global(false)` turns the crate's own loading off);
/// - a directory's `.gitignore` / `.ignore` are read by the crate when it
///   descends (`ignore-0.4.32` `walk.rs` `Walk::next`: `skip_entry`, which runs
///   `filter_entry`, comes first at line 1228, and `add_child`, which opens
///   them, only after it at line 1242; a skipped directory gets
///   `add_child_with_entries(path, &[])`, which reads nothing). `filter_entry`
///   therefore refuses a directory whose ignore file is a protected link;
/// - the root's own `.gitignore` / `.ignore`, and those of its ancestors (the
///   crate's `add_parents`, `dir.rs` line 192, reads every ancestor's even with
///   `parents(false)`), are checked before the walker exists.
///
/// `None` means the walk cannot be made safely: it yields nothing and the caller
/// marks it stopped.
fn untracked_walk(
    root: &Path,
    common_dir: &Path,
    only_under: Option<&Path>,
    home: Option<&Path>,
    counter: Arc<AtomicUsize>,
    visit_budget: usize,
) -> Option<ignore::Walk> {
    if root
        .ancestors()
        .any(|a| ignore_dir_has_protected_link(a, home))
    {
        return None;
    }
    let mut builder = WalkBuilder::new(root);
    builder
        .require_git(false)
        .hidden(false)
        .parents(false)
        .git_exclude(false)
        .git_global(false)
        // Explicit ignore files are matched relative to this directory: the
        // repo root, as git does, not the daemon's working directory.
        .current_dir(root);
    // No `sort_by_file_path`: walkdir would read and sort a whole directory
    // before yielding any of it, an unbounded allocation for a directory with
    // millions of entries. `collect_walk` sorts what it keeps instead.
    if let Some(global) = identity::global_excludes_file(home) {
        let _ = builder.add_ignore(global);
    }
    // `info/exclude` and whatever leads to it (`info` can be a link too).
    if let Ok(exclude) = identity::resolve_safe(&common_dir.join("info").join("exclude"), home) {
        let _ = builder.add_ignore(exclude);
    }
    let only = only_under.map(Path::to_path_buf);
    let home = home.map(Path::to_path_buf);
    builder.filter_entry(move |entry| {
        if entry.depth() == 0 {
            return true;
        }
        let is_dir = entry.file_type().is_some_and(|t| t.is_dir());
        // Every entry the crate offers counts toward the visit budget, kept or
        // rejected below. The first one past the budget is let through (unless
        // it is a directory whose ignore file is unsafe) so that `collect_walk`
        // receives it, sees the spent budget and stops: a rejected entry is
        // never yielded, so rejecting everything would leave the crate looping
        // over the rest of the directory.
        let over = counter.fetch_add(1, Ordering::Relaxed) >= visit_budget;
        // A protected folder (`~/Documents` under a view rooted at `$HOME`) is
        // refused lexically BEFORE any filesystem call, the `lstat` of its
        // `.gitignore` included: that stat is what macOS prompts for. The walk is
        // the sequential `ignore::Walk` (`builder.build()`), which runs this
        // closure on the caller's thread, so a session child's thread-local scope
        // (`identity::session_scope`) is visible here and a view inside the
        // session's own protected folder still walks its own tree.
        if is_tcc_path(entry.path(), home.as_deref()) {
            return false;
        }
        if over {
            return !(is_dir && ignore_dir_has_protected_link(entry.path(), home.as_deref()));
        }
        if let Some(only) = &only {
            let p = entry.path();
            if !(only.starts_with(p) || p.starts_with(only)) {
                return false;
            }
        }
        let name = entry.file_name().to_str();
        if name.is_some_and(|n| is_hard_skip_dir(n) || UNTRACKED_SKIP_DIRS.contains(&n)) {
            return false;
        }
        if !is_dir {
            return true;
        }
        // The crate opens this directory's `.gitignore` / `.ignore` once it
        // descends: a directory with one that links into a protected folder is
        // not entered.
        if ignore_dir_has_protected_link(entry.path(), home.as_deref()) {
            return false;
        }
        // A nested repo or worktree is somebody else's view. `lstat`, so a
        // `.git` that is a symlink is never followed.
        std::fs::symlink_metadata(entry.path().join(".git")).is_err()
    });
    Some(builder.build())
}

/// Is `dir/.gitignore` or `dir/.ignore` a symlink that leads into a protected
/// folder? One `lstat` each; `resolve_safe` only for an actual symlink. A
/// missing file, a regular file, or a link to an ordinary place is fine: the
/// crate may open those.
fn ignore_dir_has_protected_link(dir: &Path, home: Option<&Path>) -> bool {
    [".gitignore", ".ignore"].iter().any(|name| {
        let file = dir.join(name);
        std::fs::symlink_metadata(&file).is_ok_and(|m| m.file_type().is_symlink())
            && identity::resolve_safe(&file, home).is_err_and(|e| identity::is_protected_error(&e))
    })
}

/// Code files found by `untracked_walk` that `keep` accepts, in path order.
/// Stops (`stopped`) at the first file beyond `allowance` files, or when the walk
/// has visited `visit_budget` entries without finishing.
pub(super) fn collect_walk(
    root: &Path,
    common_dir: &Path,
    only_under: Option<&Path>,
    home: Option<&Path>,
    keep: impl Fn(&str) -> bool,
    allowance: usize,
    visit_budget: usize,
) -> WalkOutcome {
    let mut out = WalkOutcome {
        files: Vec::new(),
        stopped: false,
        visited: 0,
    };
    // Shared with the walker's `filter_entry`, which counts every entry the
    // crate offers it (kept or rejected); the walk is over once it passes the
    // budget. Entries the crate drops by ignore-pattern match before the filter
    // runs (`skip_entry` calls `should_skip_entry` first, ignore 0.4.32
    // walk.rs:1156) are never counted: reading a directory full of ignored names
    // is still unbounded work (time, not memory: nothing is sorted or collected).
    let counter = Arc::new(AtomicUsize::new(0));
    let Some(walk) = untracked_walk(
        root,
        common_dir,
        only_under,
        home,
        counter.clone(),
        visit_budget,
    ) else {
        // An ignore file of the root or an ancestor links into a protected
        // folder: no safe walk exists, so nothing is walked and the view is
        // marked truncated.
        out.stopped = true;
        return out;
    };
    for entry in walk {
        let Ok(entry) = entry else {
            // An unreadable entry is still a visit.
            if counter.fetch_add(1, Ordering::Relaxed) >= visit_budget {
                break;
            }
            continue;
        };
        if entry.depth() == 0 {
            continue;
        }
        if counter.load(Ordering::Relaxed) > visit_budget {
            break; // the entry past the budget: the walk is over
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        if only_under.is_some_and(|d| !entry.path().starts_with(d)) {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        let Some(rel) = rel.to_str() else { continue };
        if !rel_passes_caps(rel) || !keep(rel) {
            continue;
        }
        if out.files.len() >= allowance {
            out.stopped = true;
            break;
        }
        out.files.push(rel.to_string());
    }
    let seen = counter.load(Ordering::Relaxed);
    out.stopped |= seen > visit_budget;
    out.visited = seen.min(visit_budget);
    // The walker is not sorted (see `untracked_walk`): put what was kept in path
    // order, component by component, as the sorted walk yielded it.
    out.files.sort_by(|a, b| Path::new(a).cmp(Path::new(b)));
    out
}

/// Untracked code files of a worktree, in path order, at most `allowance` of
/// them; `stopped` says the walk ended early (file allowance or visit budget).
fn walk_untracked(
    view: &ViewId,
    common_dir: &Path,
    tracked: &HashSet<String>,
    home: Option<&Path>,
    allowance: usize,
    visit_budget: usize,
) -> WalkOutcome {
    collect_walk(
        &view.worktree_path,
        common_dir,
        None,
        home,
        |rel| !tracked.contains(rel),
        allowance,
        visit_budget,
    )
}

/// Is every directory above the repo-relative file `rel` safe to stat through?
/// `root` is a view root that `list_views_with` already resolved, so the first
/// level is an `lstat` inside a safe directory. A real directory is safe. A
/// symlink is resolved with `resolve_safe`: one that leads into a protected
/// folder is unsafe, and so is everything below it. A missing directory or a
/// plain file in its place is unsafe too (the file would not stat anyway).
///
/// `memo` maps a repo-relative directory to its answer, so a scan costs one
/// `lstat` per unique tracked directory however many files it holds. A tracked
/// `src/a.rs` whose `src` was swapped for a link into `~/Documents` is skipped
/// before `stat_regular` (which follows intermediate links) or a read can reach
/// the protected folder.
fn parent_dirs_safe(
    root: &Path,
    rel: &str,
    memo: &mut HashMap<String, bool>,
    home: Option<&Path>,
) -> bool {
    let Some((dir, _)) = rel.rsplit_once('/') else {
        return true; // directly in the view root
    };
    if let Some(&known) = memo.get(dir) {
        return known;
    }
    let safe = parent_dirs_safe(root, dir, memo, home) && {
        let abs = root.join(dir);
        match std::fs::symlink_metadata(&abs) {
            Ok(m) if m.file_type().is_symlink() => identity::resolve_safe(&abs, home).is_ok(),
            Ok(m) => m.is_dir(),
            Err(_) => false,
        }
    };
    memo.insert(dir.to_string(), safe);
    safe
}

fn collect_view(
    view: &ViewId,
    common_dir: &Path,
    opts: &ScanOpts,
    parser: &mut BlobParser<'_>,
    counters: &mut ScanCounters,
) -> Result<ViewScan> {
    let index = LiveIndex::read(view)?;
    let entries = index.regular_files(rel_passes_caps);
    let tracked: HashSet<String> = entries.iter().map(|e| e.path.clone()).collect();
    let mut scan = ViewScan {
        files: Vec::new(),
        truncated: false,
        skipped_large: 0,
    };
    let home = opts.home();
    // One answer per directory of the view, not per file: a tracked file is only
    // stat'ed (and read) when every directory above it is safe.
    let mut dir_memo: HashMap<String, bool> = HashMap::new();
    for e in &entries {
        let abs = view.worktree_path.join(&e.path);
        // A view rooted above a protected folder (an explicit `codemap scan
        // --repo ~`) tracks files inside it: refuse them lexically, before the
        // `lstat` of any parent, which would itself touch the folder.
        if is_tcc_path(&abs, home.as_deref()) {
            continue;
        }
        if !parent_dirs_safe(&view.worktree_path, &e.path, &mut dir_memo, home.as_deref()) {
            continue; // a link into a protected folder: skipped as if deleted
        }
        let Some(st) = stat_regular(&abs) else {
            continue; // deleted, or not a regular file any more
        };
        if st.size > MAX_FILE_BYTES {
            scan.skipped_large += 1;
            continue;
        }
        if scan.files.len() >= opts.max_files_per_view {
            scan.truncated = true;
            break;
        }
        let stat_clean = clean_by_stat(e, &st, index.mtime);
        settle_file(
            parser,
            counters,
            &e.path,
            &abs,
            &st,
            Some(&e.oid),
            stat_clean,
            &mut scan.files,
        )?;
    }
    if !scan.truncated {
        // Only as many untracked files as the cap still has room for, and a
        // bounded number of visited entries: a stopped walk is a truncated view.
        let room = opts.max_files_per_view.saturating_sub(scan.files.len());
        // No `parent_dirs_safe` check on the files this walk returns: `WalkBuilder`
        // is left at `follow_links(false)`, so a symlinked directory is yielded
        // as a symlink entry and never entered, and every returned path runs
        // through real directories only.
        let walk = walk_untracked(
            view,
            common_dir,
            &tracked,
            home.as_deref(),
            room,
            walk_visit_budget(opts.max_files_per_view),
        );
        #[cfg(test)]
        test_hooks::record_walk(walk.files.len(), walk.visited, walk.stopped);
        if walk.stopped {
            scan.truncated = true;
        }
        for rel in walk.files {
            let abs = view.worktree_path.join(&rel);
            let Some(st) = stat_regular(&abs) else {
                continue;
            };
            if st.size > MAX_FILE_BYTES {
                scan.skipped_large += 1;
                continue;
            }
            if scan.files.len() >= opts.max_files_per_view {
                scan.truncated = true;
                break;
            }
            settle_file(
                parser,
                counters,
                &rel,
                &abs,
                &st,
                None,
                false,
                &mut scan.files,
            )?;
        }
    }
    Ok(scan)
}

fn short(oid: &Option<String>) -> Option<String> {
    oid.as_ref().map(|o| o.chars().take(12).collect())
}

fn scan_view(
    conn: &Connection,
    view: &ViewId,
    common_dir: &Path,
    opts: &ScanOpts,
    counters: &mut ScanCounters,
    totals: &mut ScanTotals,
) -> Result<ViewReport> {
    let t0 = Instant::now();
    let common_str = common_dir.to_string_lossy().to_string();
    let wt_str = view.worktree_path.to_string_lossy().to_string();
    // State before reading the index: if the index moves mid-scan the stored
    // stat is older than the rows, and the next probe sees the difference.
    let state = identity::view_state(view)?;
    let mut parser = BlobParser::new(conn, &common_str);
    let scan = collect_view(view, common_dir, opts, &mut parser, counters)?;
    #[cfg(test)]
    test_hooks::after_collect();
    let parsed = parser.take();
    let ms = t0.elapsed().as_millis() as u64;
    let outcome = store::write_view(
        conn,
        &ViewWrite {
            common_dir: &common_str,
            worktree_path: &wt_str,
            branch: state.branch.as_deref(),
            head_oid: state.head_oid.as_deref(),
            index_mtime_ns: state.index_mtime_ns,
            index_size: state.index_size,
            scanned_at: super::now_secs(),
            status: "ok",
            truncated: scan.truncated,
            scan_ms: ms as i64,
        },
        &scan.files,
        &parsed,
    )?;
    totals.defs_written += outcome.defs_written;
    totals.gc_blobs += outcome.gc_blobs;
    totals.file_pairs += scan.files.len();
    Ok(ViewReport {
        path: wt_str,
        branch: state.branch,
        head: short(&state.head_oid),
        files: scan.files.len(),
        dirty: scan.files.iter().filter(|f| f.dirty == 1).count(),
        untracked: scan.files.iter().filter(|f| f.dirty == 2).count(),
        truncated: scan.truncated,
        skipped_large: scan.skipped_large,
        ms: t0.elapsed().as_millis() as u64,
        status: "ok".into(),
    })
}

/// Scan every worktree of the repo whose git common dir is `common_dir`.
///
/// Synchronous; the daemon calls it from `spawn_blocking`. Each view commits in
/// its own short transaction after its blobs are parsed, so a long scan never
/// holds the write lock while parsing. A view that fails is recorded with
/// status `error: ...` and its old rows stay; the other views still scan.
pub fn scan_repo(conn: &Connection, common_dir: &Path, opts: &ScanOpts) -> Result<RepoScanReport> {
    let t0 = Instant::now();
    let mut report = RepoScanReport {
        common_dir: common_dir.to_string_lossy().to_string(),
        ..Default::default()
    };
    if super::disabled() {
        return Ok(report);
    }
    let home = opts.home();
    if is_tcc_path(common_dir, home.as_deref()) {
        return Err(identity::ProtectedPath::new(common_dir, home.as_deref()).into());
    }
    // A repo whose common dir is gone has no views left. The check resolves
    // symlinks itself: a link into a protected folder is refused, not stat'ed.
    let listing = match identity::resolve_safe(common_dir, home.as_deref()) {
        Ok(real) if real.is_dir() => identity::list_views_with(&real, home.as_deref())?,
        Err(e) if identity::is_protected_error(&e) => return Err(e),
        _ => identity::ViewListing::default(),
    };
    report.stale_worktrees = listing.stale;
    let live: HashSet<String> = listing
        .views
        .iter()
        .map(|v| v.worktree_path.to_string_lossy().to_string())
        .collect();
    for stored in store::stored_views(conn, Some(&report.common_dir))? {
        if !live.contains(&stored.worktree_path) {
            report.totals.gc_blobs += store::delete_view(conn, stored.id)?;
            report.removed_views += 1;
        }
    }

    let mut counters = ScanCounters::default();
    for view in &listing.views {
        match scan_view(
            conn,
            view,
            common_dir,
            opts,
            &mut counters,
            &mut report.totals,
        ) {
            Ok(r) => report.views.push(r),
            Err(e) => {
                let msg: String = format!("error: {e}").chars().take(160).collect();
                let wt = view.worktree_path.to_string_lossy().to_string();
                let _ = store::mark_view_error(
                    conn,
                    &ViewWrite {
                        common_dir: &report.common_dir,
                        worktree_path: &wt,
                        branch: None,
                        head_oid: None,
                        index_mtime_ns: None,
                        index_size: None,
                        scanned_at: super::now_secs(),
                        status: &msg,
                        truncated: false,
                        scan_ms: 0,
                    },
                );
                report.views.push(ViewReport {
                    path: wt,
                    status: msg,
                    ..Default::default()
                });
            }
        }
    }
    report.totals.gc_blobs += store::gc(conn)?;
    report.totals.unique_blobs_seen = counters.seen.len();
    report.totals.blobs_parsed = counters.parsed;
    report.totals.blobs_reused = counters.reused;
    report.totals.ms = t0.elapsed().as_millis() as u64;
    let _ = store::record_poll(conn, &report.common_dir, "ok");
    Ok(report)
}

// ---- cheap probe ---------------------------------------------------------------------

/// Has this view changed since it was last fully scanned? Compares the stored
/// HEAD, branch and index-file (mtime, size) with the live ones; the index is not
/// parsed. Also true when the view was never scanned, last failed, or holds a
/// blob the current extractor has not parsed. Edits to untracked files and
/// worktree edits that do not touch the index are the watcher's job, not this
/// probe's.
pub fn view_changed(conn: &Connection, view: &ViewId) -> Result<bool> {
    let wt = view.worktree_path.to_string_lossy().to_string();
    let Some(stored) = store::stored_view_by_path(conn, &wt)? else {
        return Ok(true);
    };
    if stored.status != "ok" {
        return Ok(true);
    }
    let (branch, head) = head_of(view)?;
    if head != stored.head_oid || branch != stored.branch {
        return Ok(true);
    }
    let (mtime, size) = index_stat(view);
    if mtime != stored.index_mtime_ns || size != stored.index_size {
        return Ok(true);
    }
    store::view_has_unparsed_blobs(conn, stored.id)
}

// ---- partial rescan ---------------------------------------------------------------

pub(super) fn longest_view_for<'a>(views: &'a [StoredView], abs: &Path) -> Option<&'a StoredView> {
    views
        .iter()
        .filter(|v| abs.starts_with(&v.worktree_path))
        .max_by_key(|v| Path::new(&v.worktree_path).components().count())
}

/// Is an untracked file excluded from a view (tool-cache directory, nested repo,
/// or ignored)? Same rules as the full scan's walk.
fn untracked_excluded(root: &Path, common_dir: &Path, rel: &str) -> bool {
    let parts: Vec<&str> = rel.split('/').collect();
    if parts[..parts.len() - 1]
        .iter()
        .any(|p| UNTRACKED_SKIP_DIRS.contains(p))
    {
        return true;
    }
    // Directories between the root and the file.
    let mut dir = root.to_path_buf();
    let mut levels: Vec<PathBuf> = vec![dir.clone()];
    for part in &parts[..parts.len() - 1] {
        dir = dir.join(part);
        if std::fs::symlink_metadata(dir.join(".git")).is_ok() {
            return true;
        }
        levels.push(dir.clone());
    }
    let abs = root.join(rel);
    // Lowest precedence first: global ignore, info/exclude, then each .gitignore
    // from the root down; the deepest verdict wins.
    let mut matchers = Vec::new();
    let global = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| identity::home_dir().map(|h| h.join(".config")))
        .map(|c| c.join("git").join("ignore"));
    let mut sources: Vec<(PathBuf, PathBuf)> = Vec::new();
    // Ignore files may be symlinks (dotfile managers): follow them only through
    // `resolve_safe`, never into a protected folder.
    let home = identity::home_dir();
    let home = home.as_deref();
    if let Some(g) = global.filter(|g| identity::is_file_safe(g, home)) {
        sources.push((root.to_path_buf(), g));
    }
    let exclude = common_dir.join("info").join("exclude");
    if identity::is_file_safe(&exclude, home) {
        sources.push((root.to_path_buf(), exclude));
    }
    for d in &levels {
        let gi = d.join(".gitignore");
        if identity::is_file_safe(&gi, home) {
            sources.push((d.clone(), gi));
        }
    }
    for (base, file) in sources {
        let mut b = GitignoreBuilder::new(&base);
        let _ = b.add(&file);
        if let Ok(g) = b.build() {
            matchers.push((base, g));
        }
    }
    for (base, g) in matchers.iter().rev() {
        let Ok(under) = abs.strip_prefix(base) else {
            continue;
        };
        match g.matched_path_or_any_parents(under, false) {
            ignore::Match::Ignore(_) => return true,
            ignore::Match::Whitelist(_) => return false,
            ignore::Match::None => {}
        }
    }
    false
}

// ---- directory events ----------------------------------------------------------------

/// Can an event on this repo-relative path with no file extension be a
/// directory that was renamed, created or removed? Every segment, the last
/// included, must clear the hard-skip and tool-cache lists (`target`,
/// `node_modules`, `.git`, `.venv` ...). Lexical.
pub(crate) fn dir_event_rel_ok(rel: &str) -> bool {
    !rel.is_empty()
        && rel
            .split('/')
            .all(|s| !s.is_empty() && !is_hard_skip_dir(s) && !UNTRACKED_SKIP_DIRS.contains(&s))
}

/// What `expand_dir_events` did.
#[derive(Debug, Default)]
pub(crate) struct DirExpansion {
    /// The paths to hand to `rescan_paths`: the code paths given, plus the
    /// stored and the on-disk code files below every directory event.
    pub paths: Vec<PathBuf>,
    /// Extension-less paths inside a view that were looked at.
    pub dir_events: usize,
}

/// Turn the extension-less paths of a watcher batch (a renamed, created or
/// removed directory) into the code files below them, so `rescan_paths` can drop
/// the rows of the old location and add the new one. Code paths pass through.
///
/// For each such path inside a stored view: one `lstat`, and one indexed range
/// query for the stored rows below it (a plain file such as `4913` or `Makefile`
/// costs nothing more). If it is a directory now, its code files are added from
/// a bounded walk with the untracked walk's ignore rules, at most the view's
/// remaining file allowance, never following a symlink and never entering a
/// protected folder.
pub(crate) fn expand_dir_events(conn: &Connection, abs_paths: &[PathBuf]) -> Result<DirExpansion> {
    expand_dir_events_with(conn, abs_paths, identity::home_dir().as_deref())
}

/// `lstat` says this path is a directory (a symlink is not). The only `lstat` of
/// a watcher event path; tests record the paths it is asked about.
fn lstat_is_dir(abs: &Path) -> bool {
    #[cfg(test)]
    test_hooks::record_lstat(abs);
    std::fs::symlink_metadata(abs).is_ok_and(|m| m.is_dir())
}

pub(crate) fn expand_dir_events_with(
    conn: &Connection,
    abs_paths: &[PathBuf],
    home: Option<&Path>,
) -> Result<DirExpansion> {
    let mut out: BTreeSet<PathBuf> = BTreeSet::new();
    let mut result = DirExpansion::default();
    let mut views: Option<Vec<StoredView>> = None;
    let home = home.map(Path::to_path_buf);
    for p in abs_paths {
        if has_code_extension(p) {
            out.insert(p.clone());
            continue;
        }
        if p.extension().is_some() {
            continue;
        }
        let abs = normalize_lexical(p);
        if is_tcc_path(&abs, home.as_deref()) {
            continue;
        }
        if views.is_none() {
            views = Some(store::stored_views(conn, None)?);
        }
        let Some(view) = views.as_deref().and_then(|vs| longest_view_for(vs, &abs)) else {
            continue;
        };
        let Some(rel) = abs
            .strip_prefix(&view.worktree_path)
            .ok()
            .and_then(|r| r.to_str())
            .filter(|r| dir_event_rel_ok(r))
        else {
            continue;
        };
        result.dir_events += 1;
        let root = PathBuf::from(&view.worktree_path);
        // Rows below the path (it may be gone, or now a file): rescan_paths
        // drops those whose file no longer exists.
        let stored = store::stored_paths_under(conn, view.id, rel)?;
        out.extend(stored.iter().map(|s| root.join(s)));
        // Resolve BEFORE the `lstat`: `/repo/src/lib` with `/repo/src` swapped for
        // a link into ~/Documents would otherwise be stat'ed through the link.
        // `resolve_safe` refuses at the first protected prefix without a stat
        // inside it; any error (protected or missing) ends this event.
        if identity::resolve_safe(&abs, home.as_deref()).is_err() {
            continue;
        }
        // `lstat`: a symlink is not a directory to the full scan either.
        if !lstat_is_dir(&abs) {
            continue;
        }
        let room = (MAX_FILES_PER_PROJECT as i64 - view.files).max(0) as usize + stored.len();
        let found = collect_walk(
            &root,
            Path::new(&view.common_dir),
            Some(&abs),
            home.as_deref(),
            |_| true,
            room,
            walk_visit_budget(MAX_FILES_PER_PROJECT),
        );
        out.extend(found.files.iter().map(|f| root.join(f)));
    }
    result.paths = out.into_iter().collect();
    Ok(result)
}

/// Redo steps 2-5 of the scan for these absolute paths only, in the views that
/// hold them. A path maps to the stored view with the longest matching worktree
/// path, so a file under `.claude/worktrees/x/` lands in view `x`, not in the
/// main worktree. Paths outside every stored view, in protected folders, or
/// failing the caps are skipped. A deleted (or now oversized) file drops its row.
///
/// Cost is O(paths): the touched view's index is parsed only when a path's index
/// oid cannot be derived from its stored row (a modified or new file, or an index
/// that moved since the last scan). Views no path maps to are not read.
pub fn rescan_paths(conn: &Connection, abs_paths: &[PathBuf]) -> Result<PathScanReport> {
    rescan_paths_with(conn, abs_paths, &|_| Some(Box::new(())))
}

/// What `rescan_paths_with` calls before it touches a repo's views: returns the
/// guard that keeps the repo exclusive while its paths are rescanned (dropped
/// afterwards), or `None` to leave that repo's paths alone (another scanner holds
/// it; the repo lands in `PathScanReport::locked_repos`).
pub type RepoGuard<'a> = &'a dyn Fn(&str) -> Option<Box<dyn std::any::Any>>;

/// `rescan_paths`, taking `guard` for each repo before any of its files is read,
/// so a path rescan and a full scan of the same repo never interleave.
pub fn rescan_paths_with(
    conn: &Connection,
    abs_paths: &[PathBuf],
    guard: RepoGuard<'_>,
) -> Result<PathScanReport> {
    let t0 = Instant::now();
    let mut report = PathScanReport {
        paths: abs_paths.len(),
        ..Default::default()
    };
    if super::disabled() || abs_paths.is_empty() {
        return Ok(report);
    }
    let home = identity::home_dir();
    let views = store::stored_views(conn, None)?;
    let mut by_view: BTreeMap<i64, (StoredView, BTreeSet<String>)> = BTreeMap::new();
    for p in abs_paths {
        let abs = normalize_lexical(p);
        if is_tcc_path(&abs, home.as_deref()) {
            report.skipped_protected += 1;
            continue;
        }
        let resolve = |abs: &Path| -> Option<(&StoredView, String)> {
            let v = longest_view_for(&views, abs)?;
            let rel = abs.strip_prefix(&v.worktree_path).ok()?.to_str()?;
            Some((v, rel.to_string()))
        };
        // Hook paths can arrive through a symlink (/tmp, /var): retry on the
        // real parent directory. `resolve_safe` never stats a protected folder.
        let hit = resolve(&abs).or_else(|| {
            let real = identity::resolve_safe(abs.parent()?, home.as_deref())
                .ok()?
                .join(abs.file_name()?);
            resolve(&real)
        });
        let Some((v, rel)) = hit else {
            report.skipped_outside_views += 1;
            continue;
        };
        by_view
            .entry(v.id)
            .or_insert_with(|| (v.clone(), BTreeSet::new()))
            .1
            .insert(rel);
    }

    for (_, (view, rels)) in by_view {
        // Held for this view's read and write, dropped before the next view.
        let Some(_held) = guard(&view.common_dir) else {
            if !report.locked_repos.contains(&view.common_dir) {
                report.locked_repos.push(view.common_dir.clone());
            }
            continue;
        };
        rescan_view_paths(conn, &view, &rels, &mut report)?;
        report.views_touched.push(view.worktree_path.clone());
    }
    report.ms = t0.elapsed().as_millis() as u64;
    Ok(report)
}

fn rescan_view_paths(
    conn: &Connection,
    view: &StoredView,
    rels: &BTreeSet<String>,
    report: &mut PathScanReport,
) -> Result<()> {
    let root = PathBuf::from(&view.worktree_path);
    let home = identity::home_dir();
    let git_dir = identity::git_dir_of_worktree(&root, home.as_deref())?;
    let vid = ViewId {
        worktree_path: root.clone(),
        git_dir,
        is_main: false,
    };
    let common_dir = PathBuf::from(&view.common_dir);
    let (live_mtime, live_size) = index_stat(&vid);
    let index_unmoved = live_mtime == view.index_mtime_ns && live_size == view.index_size;
    let mut index: Option<LiveIndex> = None;
    let mut parser = BlobParser::new(conn, &view.common_dir);
    let mut changes: Vec<(String, Option<FileRec>)> = Vec::new();
    let mut additions = 0i64;
    let mut truncated = false;
    let cap = MAX_FILES_PER_PROJECT as i64;

    for rel in rels {
        if !rel_passes_caps(rel) {
            report.skipped_caps += 1;
            continue;
        }
        let abs = root.join(rel);
        // A directory inside the view can be a symlink into a protected folder
        // (the full scan never follows one, a hook path can name one): stat
        // nothing under it.
        if let Some(parent) = abs.parent() {
            if identity::resolve_safe(parent, home.as_deref())
                .is_err_and(|e| identity::is_protected_error(&e))
            {
                report.skipped_protected += 1;
                continue;
            }
        }
        let stored = store::stored_file(conn, view.id, rel)?;
        let st = stat_regular(&abs).filter(|s| s.size <= MAX_FILE_BYTES);
        let bytes = st.as_ref().and_then(|_| read_capped(&abs));
        let (Some(st), Some(bytes)) = (st, bytes) else {
            // Gone, no longer a regular file, or over the size cap.
            if stored.is_some() {
                changes.push((rel.clone(), None));
                report.removed += 1;
            } else {
                report.skipped_caps += 1;
            }
            continue;
        };
        let oid = git_blob_oid(&bytes);
        let index_oid: Option<String> = match &stored {
            Some(s) if s.dirty == 0 && index_unmoved => Some(s.blob_oid.clone()),
            _ => {
                if index.is_none() {
                    index = Some(LiveIndex::read(&vid)?);
                }
                index.as_ref().and_then(|i| i.oid_of(rel))
            }
        };
        let dirty = match &index_oid {
            Some(i) if *i == oid => 0,
            Some(_) => 1,
            None => 2,
        };
        if dirty == 2 && untracked_excluded(&root, &common_dir, rel) {
            if stored.is_some() {
                changes.push((rel.clone(), None));
                report.removed += 1;
            } else {
                report.skipped_ignored += 1;
            }
            continue;
        }
        if stored.is_none() && view.files + additions >= cap {
            truncated = true;
            report.skipped_caps += 1;
            continue;
        }
        let rec = FileRec {
            path: rel.clone(),
            blob_oid: oid.clone(),
            dirty,
            size: st.size as i64,
            mtime_ns: st.mtime_ns(),
        };
        if stored.as_ref() == Some(&rec) {
            report.unchanged += 1;
            continue;
        }
        if !parser.is_known(&oid, rel)? {
            parser.parse(&oid, rel, &bytes);
        }
        if stored.is_none() {
            additions += 1;
        }
        changes.push((rel.clone(), Some(rec)));
        report.updated += 1;
    }

    let parsed = parser.take();
    report.blobs_parsed += parsed.len();
    if changes.is_empty() && !truncated {
        return Ok(());
    }
    let outcome = store::apply_path_changes(conn, view.id, &changes, &parsed, truncated)?;
    report.defs_written += outcome.defs_written;
    report.gc_blobs += outcome.gc_blobs;
    Ok(())
}
