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

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Result};
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
    pub ms: u64,
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
    if lang_from_path_str(rel).is_none() {
        return blob("no_lang", None, Vec::new());
    }
    if looks_minified(Path::new(rel), source) {
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
    /// Blob oids known stored (current extractor version) or parsed here.
    known: HashSet<String>,
    parsed: Vec<ParsedBlob>,
}

impl<'a> BlobParser<'a> {
    fn new(conn: &'a Connection, common_dir: &'a str) -> Self {
        Self {
            conn,
            common_dir,
            known: HashSet::new(),
            parsed: Vec::new(),
        }
    }

    fn is_known(&mut self, oid: &str) -> Result<bool> {
        if self.known.contains(oid) {
            return Ok(true);
        }
        if store::blob_known(self.conn, oid)? {
            self.known.insert(oid.to_string());
            return Ok(true);
        }
        Ok(false)
    }

    fn parse(&mut self, oid: &str, rel: &str, bytes: &[u8]) {
        self.parsed
            .push(parse_blob(oid, rel, bytes, self.common_dir));
        self.known.insert(oid.to_string());
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
    if parser.is_known(&oid)? {
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
        if !parser.is_known(&oid)? {
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

/// Untracked code files of a worktree, in path order: `.gitignore`,
/// `.git/info/exclude` and the user's global ignore apply; hard-skip
/// directories, tool-cache dot directories and any directory with its own `.git`
/// are not entered.
fn walk_untracked(view: &ViewId, common_dir: &Path, tracked: &HashSet<String>) -> Vec<String> {
    let mut builder = WalkBuilder::new(&view.worktree_path);
    builder
        .require_git(false)
        .hidden(false)
        .parents(false)
        .git_exclude(false)
        .sort_by_file_path(|a, b| a.cmp(b));
    let _ = builder.add_ignore(common_dir.join("info").join("exclude"));
    builder.filter_entry(|entry| {
        if entry.depth() == 0 {
            return true;
        }
        let is_dir = entry.file_type().is_some_and(|t| t.is_dir());
        let name = entry.file_name().to_str();
        if name.is_some_and(|n| is_hard_skip_dir(n) || UNTRACKED_SKIP_DIRS.contains(&n)) {
            return false;
        }
        // A nested repo or worktree is somebody else's view. `lstat`, so a
        // `.git` that is a symlink is never followed.
        !(is_dir && std::fs::symlink_metadata(entry.path().join(".git")).is_ok())
    });
    let mut out = Vec::new();
    for entry in builder.build().flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(&view.worktree_path) else {
            continue;
        };
        let Some(rel) = rel.to_str() else { continue };
        if !rel_passes_caps(rel) || tracked.contains(rel) {
            continue;
        }
        out.push(rel.to_string());
    }
    out
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
    for e in &entries {
        let abs = view.worktree_path.join(&e.path);
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
        for rel in walk_untracked(view, common_dir, &tracked) {
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
        bail!(
            "refusing {}: macOS-protected folder (Desktop, Documents, Downloads)",
            common_dir.display()
        );
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

fn longest_view_for<'a>(views: &'a [StoredView], abs: &Path) -> Option<&'a StoredView> {
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
        if !parser.is_known(&oid)? {
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
