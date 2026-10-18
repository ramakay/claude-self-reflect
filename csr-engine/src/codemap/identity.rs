//! Repo and worktree identity, and the git facts the scanner needs.
//!
//! Identity is the git COMMON dir (realpath): one value for a main checkout and
//! every linked worktree of it. It is derived by reading `.git` / `commondir`
//! files, never by `canonical_repo_path`, which collapses worktrees onto the
//! main path and so cannot tell views apart.
//!
//! Git facts (HEAD, branch, index entries) come from `gix` opened in isolated
//! mode: no user config, no environment, no hooks, nothing is written.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

/// Where a path sits in git.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoId {
    /// realpath of the git common dir (`<main>/.git` for a normal checkout).
    pub common_dir: PathBuf,
    /// realpath of the worktree root the path is inside.
    pub worktree_root: PathBuf,
}

/// One worktree of a repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewId {
    /// realpath of the worktree root.
    pub worktree_path: PathBuf,
    /// The git dir that holds this worktree's HEAD and index: the common dir for
    /// the main worktree, `<common>/worktrees/<name>` for a linked one.
    pub git_dir: PathBuf,
    pub is_main: bool,
}

/// The views of a repo, plus how many registered worktrees were skipped.
#[derive(Debug, Clone, Default)]
pub struct ViewListing {
    pub views: Vec<ViewId>,
    /// `<common>/worktrees/*` entries whose directory is gone (prunable), whose
    /// pointer is unreadable, or that sit in a protected folder.
    pub stale: usize,
}

/// HEAD and index-file facts for a view.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ViewState {
    /// Short branch name; `None` when HEAD is detached or unborn.
    pub branch: Option<String>,
    /// Full hex HEAD commit oid; `None` for an unborn branch.
    pub head_oid: Option<String>,
    pub index_mtime_ns: Option<i64>,
    pub index_size: Option<i64>,
}

/// One regular-file entry of a view's index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    pub path: String,
    pub oid: String,
    pub mtime_secs: u32,
    pub mtime_nsecs: u32,
    pub size: u32,
}

/// A parsed index plus the index file's own mtime.
pub struct LiveIndex {
    file: gix::index::File,
    /// mtime of the index file when it was read, `(secs, nsecs)`.
    pub mtime: Option<(u32, u32)>,
}

const TCC_FOLDERS: [&str; 3] = ["desktop", "documents", "downloads"];
const TEMP_ROOTS: [&str; 4] = [
    "/private/tmp",
    "/tmp",
    "/var/folders",
    "/private/var/folders",
];

/// Resolve `.` and `..` without touching the filesystem. A relative path is
/// anchored at the current directory.
pub fn normalize_lexical(path: &Path) -> PathBuf {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    };
    let mut out = PathBuf::new();
    for comp in abs.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The protected folder a path sits in: what to show in a refusal, and a
/// case-folded key that is the same for every spelling of the folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TccRoot {
    /// The protected folder as spelled in the path (original case).
    pub shown: PathBuf,
    /// Lower-cased, `/System/Volumes/Data` firmlink stripped: `/users/u/desktop`,
    /// `/users/u/library/mobile documents`, `/volumes/<name>`.
    pub key: String,
}

fn lower_components(path: &Path) -> Vec<String> {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect()
}

/// Number of leading components that are the `/System/Volumes/Data` firmlink
/// (`/System/Volumes/Data/Users/u/Desktop` is `/Users/u/Desktop`), 0 if none.
fn firmlink_len(comps: &[String]) -> usize {
    if comps.len() >= 4 && comps[1] == "system" && comps[2] == "volumes" && comps[3] == "data" {
        3
    } else {
        0
    }
}

/// The protected folder `path` is, or is inside, or `None`. Protected:
///
/// - `<home>/Desktop`, `<home>/Documents`, `<home>/Downloads`, for `/Users/<u>`
///   and for the given `home`;
/// - iCloud Drive, `<home>/Library/Mobile Documents` (also spelled
///   `/Users/<u>/Library/Mobile Documents` with no `home`);
/// - any path at or below `/Volumes/<name>` (removable and network volumes are
///   their own macOS privacy classes; `/Volumes` itself is not protected);
/// - the firmlink spellings of the first two, `/System/Volumes/Data/Users/...`.
///
/// Lexical and case-folded (APFS): the filesystem is never consulted.
pub fn tcc_root(path: &Path, home: Option<&Path>) -> Option<TccRoot> {
    let norm = normalize_lexical(path);
    let orig: Vec<String> = norm
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    let comps = lower_components(&norm);
    let skip = firmlink_len(&comps);
    // Components with the firmlink removed; `at(i)` maps back to `orig`.
    let eff: Vec<&str> = std::iter::once("/")
        .chain(comps.iter().skip(1 + skip).map(String::as_str))
        .collect();
    let n = eff.len();
    let take = if n >= 3 && eff[1] == "volumes" {
        Some(3)
    } else if n >= 4 && eff[1] == "users" && TCC_FOLDERS.contains(&eff[3]) {
        Some(4)
    } else if n >= 5 && eff[1] == "users" && eff[3] == "library" && eff[4] == "mobile documents" {
        Some(5)
    } else if let Some(home) = home {
        let h = lower_components(&normalize_lexical(home));
        let hs = firmlink_len(&h);
        let h: Vec<&str> = std::iter::once("/")
            .chain(h.iter().skip(1 + hs).map(String::as_str))
            .collect();
        let hl = h.len();
        if n > hl && eff[..hl] == h[..] {
            if TCC_FOLDERS.contains(&eff[hl]) {
                Some(hl + 1)
            } else if n > hl + 1 && eff[hl] == "library" && eff[hl + 1] == "mobile documents" {
                Some(hl + 2)
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    }?;
    let key = if take == 1 {
        "/".to_string()
    } else {
        format!("/{}", eff[1..take].join("/"))
    };
    let mut shown = PathBuf::from("/");
    for part in orig.iter().skip(1).take(skip + take - 1) {
        shown.push(part);
    }
    Some(TccRoot { shown, key })
}

/// True when `path` is, or is inside, a macOS-protected folder (see `tcc_root`).
/// Lexical only: the filesystem is not consulted, so calling this never
/// triggers a macOS privacy prompt.
pub fn is_tcc_path(path: &Path, home: Option<&Path>) -> bool {
    tcc_root(path, home).is_some()
}

/// True when `path` is inside a temp root (`/private/tmp`, `/tmp`,
/// `/var/folders`, `/private/var/folders`). Lexical.
pub fn is_temp_path(path: &Path) -> bool {
    let norm = normalize_lexical(path);
    TEMP_ROOTS
        .iter()
        .any(|root| norm == Path::new(root) || norm.starts_with(root))
}

/// The error `refuse_if_protected` and `resolve_safe` return for a protected
/// folder. Callers tell "refused" from "missing" with `is_protected_error`.
#[derive(Debug)]
pub struct ProtectedPath {
    pub path: PathBuf,
    /// The protected folder it sits in, as spelled in the path.
    pub root: PathBuf,
}

impl ProtectedPath {
    /// The refusal for `path`, which must be protected (`tcc_root` is `Some`);
    /// the root falls back to the path itself otherwise.
    pub fn new(path: &Path, home: Option<&Path>) -> Self {
        let root = tcc_root(path, home)
            .map(|r| r.shown)
            .unwrap_or_else(|| path.to_path_buf());
        Self {
            path: path.to_path_buf(),
            root,
        }
    }
}

impl std::fmt::Display for ProtectedPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "refusing {}: macOS-protected folder {} (Desktop, Documents, Downloads, iCloud Drive, /Volumes)",
            self.path.display(),
            self.root.display()
        )
    }
}

impl std::error::Error for ProtectedPath {}

/// True when `err` is a protected-folder refusal.
pub fn is_protected_error(err: &anyhow::Error) -> bool {
    err.downcast_ref::<ProtectedPath>().is_some()
}

fn refuse_if_protected(path: &Path, home: Option<&Path>) -> Result<()> {
    if is_tcc_path(path, home) {
        return Err(ProtectedPath::new(path, home).into());
    }
    Ok(())
}

/// Most symbolic links `resolve_safe` follows before it gives up.
pub const MAX_SYMLINK_HOPS: usize = 40;

/// `std::fs::canonicalize` that never touches a protected folder.
///
/// `canonicalize` (realpath) lstat()s every component it resolves, including a
/// symlink's target, so a link into `~/Desktop` fires the macOS privacy prompt
/// for the binary. This resolves the path one component at a time with
/// `symlink_metadata` + `read_link`, and runs the lexical `is_tcc_path` check on
/// each resolved prefix BEFORE any lstat or `read_link` under it; the first
/// protected prefix is refused (`is_protected_error`). A relative link target
/// resolves against the already-resolved parent, `..` pops the resolved prefix
/// (so it is correct after a symlink, unlike a lexical collapse), more than
/// `MAX_SYMLINK_HOPS` links (a loop included) is refused, and a missing
/// component is an `Err` (same contract as `canonicalize`).
///
/// `home` is compared as given: pass a real path, not one that itself goes
/// through a symlink. A path whose lexical form is protected is refused up front,
/// before the walk, so a `..` that would only lexically land in a protected
/// folder is refused too (the safe direction).
pub fn resolve_safe(path: &Path, home: Option<&Path>) -> Result<PathBuf> {
    refuse_if_protected(&normalize_lexical(path), home)?;
    let anchored = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    };
    let mut pending: Vec<OsString> = Vec::new();
    push_components(&mut pending, &anchored);
    let mut cur = PathBuf::from("/");
    let mut hops = 0usize;
    while let Some(name) = pending.pop() {
        if name == ".." {
            cur.pop();
            continue;
        }
        let next = cur.join(&name);
        refuse_if_protected(&next, home)?;
        let meta = std::fs::symlink_metadata(&next)
            .with_context(|| format!("resolve {}", next.display()))?;
        if !meta.file_type().is_symlink() {
            cur = next;
            continue;
        }
        hops += 1;
        if hops > MAX_SYMLINK_HOPS {
            bail!(
                "refusing {}: more than {MAX_SYMLINK_HOPS} symbolic links (a loop?)",
                path.display()
            );
        }
        let target =
            std::fs::read_link(&next).with_context(|| format!("read link {}", next.display()))?;
        if target.is_absolute() {
            cur = PathBuf::from("/");
        }
        push_components(&mut pending, &target);
    }
    Ok(cur)
}

/// Push the components of `path` onto a stack so that popping yields them in
/// path order. `.` and the root are dropped; `..` is kept.
fn push_components(stack: &mut Vec<OsString>, path: &Path) {
    for comp in path.components().rev() {
        match comp {
            Component::Normal(n) => stack.push(n.to_os_string()),
            Component::ParentDir => stack.push(OsString::from("..")),
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
        }
    }
}

/// `path.is_dir()` for a path that may sit behind symlinks: false when it is
/// missing, not a directory, or leads into a protected folder. Never stats a
/// protected folder.
pub fn is_dir_safe(path: &Path, home: Option<&Path>) -> bool {
    resolve_safe(path, home).is_ok_and(|real| real.is_dir())
}

/// `path.is_file()`, with the same guarantees as `is_dir_safe`.
pub fn is_file_safe(path: &Path, home: Option<&Path>) -> bool {
    resolve_safe(path, home).is_ok_and(|real| real.is_file())
}

/// `std::fs::metadata` that resolves symlinks through `resolve_safe` first.
/// Returns the resolved path with its metadata.
fn metadata_safe(path: &Path, home: Option<&Path>) -> Result<(PathBuf, std::fs::Metadata)> {
    let real = resolve_safe(path, home)?;
    let meta = std::fs::metadata(&real).with_context(|| format!("stat {}", real.display()))?;
    Ok((real, meta))
}

/// Read a text file behind possible symlinks without touching a protected folder.
fn read_to_string_safe(path: &Path, home: Option<&Path>) -> Result<String> {
    let real = resolve_safe(path, home)?;
    std::fs::read_to_string(&real).with_context(|| format!("read {}", real.display()))
}

pub(crate) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// Find the repo and worktree a path is in. See `identify_with`.
pub fn identify(path: &Path) -> Result<RepoId> {
    identify_with(path, home_dir().as_deref())
}

/// Walk up from `path` to the nearest `.git` (a directory, or a file with a
/// `gitdir:` pointer; linked worktrees follow `<gitdir>/commondir`). Refuses
/// protected folders and `$HOME` itself, before any filesystem call.
pub fn identify_with(path: &Path, home: Option<&Path>) -> Result<RepoId> {
    let abs = normalize_lexical(path);
    refuse_if_protected(&abs, home)?;
    if let Some(home) = home {
        if abs == normalize_lexical(home) {
            bail!("refusing {}: that is the home directory", abs.display());
        }
    }
    // The deepest existing directory at or above `abs`, resolved without
    // stat'ing a protected folder (a symlink into one is refused, not followed).
    let mut probe = abs.clone();
    let mut dir = loop {
        match resolve_safe(&probe, home) {
            Ok(real) if real.is_dir() => break real,
            Err(e) if is_protected_error(&e) => return Err(e),
            _ => {}
        }
        match probe.parent() {
            Some(p) => probe = p.to_path_buf(),
            None => bail!("{} does not exist", abs.display()),
        }
    };
    loop {
        if let Some(home) = home {
            if dir == normalize_lexical(home) {
                bail!(
                    "{} is not inside a git repository below $HOME",
                    abs.display()
                );
            }
        }
        let dot_git = dir.join(".git");
        let found = match metadata_safe(&dot_git, home) {
            Ok(found) => Some(found),
            Err(e) if is_protected_error(&e) => return Err(e),
            Err(_) => None,
        };
        if let Some((dot_real, meta)) = found {
            let common = if meta.is_dir() {
                dot_real
            } else {
                common_dir_from_pointer(&dir, &dot_real, home)?
            };
            let common_dir = resolve_safe(&common, home)
                .with_context(|| format!("resolve {}", common.display()))?;
            // `dir` came out of `resolve_safe`: it is already the real path.
            let worktree_root = dir;
            refuse_if_protected(&common_dir, home)?;
            refuse_if_protected(&worktree_root, home)?;
            return Ok(RepoId {
                common_dir,
                worktree_root,
            });
        }
        match dir.parent() {
            Some(p) if p != dir => dir = p.to_path_buf(),
            _ => bail!("{} is not inside a git repository", abs.display()),
        }
    }
}

/// Read a `.git` pointer file and return the repo's common dir. `dir` is a
/// resolved directory; `dot_git` its resolved `.git` file.
fn common_dir_from_pointer(dir: &Path, dot_git: &Path, home: Option<&Path>) -> Result<PathBuf> {
    let text = read_to_string_safe(dot_git, home)?;
    let target = text
        .lines()
        .find_map(|l| l.strip_prefix("gitdir:"))
        .map(str::trim)
        .ok_or_else(|| anyhow!("{} has no gitdir: line", dot_git.display()))?;
    let gitdir = resolve_safe(&dir.join(target), home)?;
    let commondir_file = gitdir.join("commondir");
    match read_to_string_safe(&commondir_file, home) {
        Ok(rel) => resolve_safe(&gitdir.join(rel.trim()), home),
        Err(e) if is_protected_error(&e) => Err(e),
        // A submodule's pointer names its own git dir, which is its common dir.
        Err(_) => Ok(gitdir),
    }
}

/// The git dir that holds a worktree's HEAD and index: `<root>/.git` for a
/// normal checkout, the `gitdir:` target for a linked worktree or submodule.
pub fn git_dir_of_worktree(root: &Path, home: Option<&Path>) -> Result<PathBuf> {
    let root = resolve_safe(root, home)?;
    let dot_git = root.join(".git");
    let (dot_real, meta) =
        metadata_safe(&dot_git, home).with_context(|| format!("{} has no .git", root.display()))?;
    if meta.is_dir() {
        return Ok(dot_real);
    }
    let text = read_to_string_safe(&dot_real, home)?;
    let target = text
        .lines()
        .find_map(|l| l.strip_prefix("gitdir:"))
        .map(str::trim)
        .ok_or_else(|| anyhow!("{} has no gitdir: line", dot_git.display()))?;
    resolve_safe(&root.join(target), home)
}

fn isolated_open_options() -> gix::open::Options {
    gix::open::Options::isolated()
}

/// What `main_worktree_path` found.
enum MainWorktree {
    /// Bare repo, or no worktree configured, or it does not exist.
    None,
    /// The configured worktree is in (or leads into) a protected folder.
    Protected,
    Path(PathBuf),
}

/// `core.bare` and `core.worktree` from a git dir's own `config`, read with a
/// line scanner: `gix::open` stats the configured worktree while opening (its
/// "is this a directory" warning and its ownership check), which would hit a
/// protected folder before this module could look at the value.
fn read_core_config(config_path: &Path, home: Option<&Path>) -> (bool, Option<String>) {
    let Ok(text) = read_to_string_safe(config_path, home) else {
        return (false, None);
    };
    let mut in_core = false;
    let mut bare = false;
    let mut worktree = None;
    for raw in text.lines() {
        let mut line = raw.trim();
        if let Some(rest) = line.strip_prefix('[') {
            let Some((section, after)) = rest.split_once(']') else {
                continue;
            };
            in_core = section.trim().eq_ignore_ascii_case("core");
            line = after.trim();
            if line.is_empty() {
                continue;
            }
        }
        if !in_core {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = unquote_config_value(value);
        match key.trim().to_ascii_lowercase().as_str() {
            "bare" => bare = value.eq_ignore_ascii_case("true"),
            "worktree" => worktree = Some(value),
            _ => {}
        }
    }
    (bare, worktree)
}

/// A config value without its trailing comment and surrounding quotes.
fn unquote_config_value(raw: &str) -> String {
    let raw = raw.trim();
    if let Some(inner) = raw.strip_prefix('"') {
        if let Some(end) = inner.find('"') {
            return inner[..end].to_string();
        }
    }
    let cut = raw.find([';', '#']).unwrap_or(raw.len());
    raw[..cut].trim().to_string()
}

/// Where a repo's main worktree is. Never stats a protected folder: the
/// configured worktree is checked lexically, then resolved with `resolve_safe`,
/// before anything opens or stats it.
fn main_worktree_path(common_dir: &Path, home: Option<&Path>) -> MainWorktree {
    let candidate = if common_dir.file_name().is_some_and(|n| n == ".git") {
        match common_dir.parent() {
            Some(p) => p.to_path_buf(),
            None => return MainWorktree::None,
        }
    } else {
        // Submodule git dirs and unusual layouts: `core.worktree`, relative to
        // the git dir. No `core.worktree` (or `core.bare = true`) means no
        // worktree, as `gix` decides for a git dir not named `.git`.
        let (bare, worktree) = read_core_config(&common_dir.join("config"), home);
        match worktree.filter(|_| !bare) {
            Some(w) => common_dir.join(w),
            None => return MainWorktree::None,
        }
    };
    let candidate = normalize_lexical(&candidate);
    match resolve_safe(&candidate, home) {
        Ok(real) => MainWorktree::Path(real),
        Err(e) if is_protected_error(&e) => MainWorktree::Protected,
        Err(_) => MainWorktree::None,
    }
}

/// The main worktree (unless the repo is bare) plus every linked worktree whose
/// directory still exists. Stale entries are skipped and counted.
pub fn list_views(common_dir: &Path) -> Result<ViewListing> {
    list_views_with(common_dir, home_dir().as_deref())
}

pub fn list_views_with(common_dir: &Path, home: Option<&Path>) -> Result<ViewListing> {
    refuse_if_protected(common_dir, home)?;
    let mut listing = ViewListing::default();
    let common_dir = match resolve_safe(common_dir, home) {
        Ok(real) => real,
        Err(e) if is_protected_error(&e) => return Err(e),
        // A common dir that is gone has no views.
        Err(_) => return Ok(listing),
    };
    let common_dir = common_dir.as_path();
    let mut seen: HashSet<PathBuf> = HashSet::new();

    match main_worktree_path(common_dir, home) {
        MainWorktree::Protected => listing.stale += 1,
        MainWorktree::Path(main) if main.is_dir() => {
            seen.insert(main.clone());
            listing.views.push(ViewId {
                worktree_path: main,
                git_dir: common_dir.to_path_buf(),
                is_main: true,
            });
        }
        MainWorktree::Path(_) | MainWorktree::None => {}
    }

    let linked_root = common_dir.join("worktrees");
    let mut names: Vec<PathBuf> = match std::fs::read_dir(&linked_root) {
        Ok(rd) => rd.filter_map(|e| e.ok()).map(|e| e.path()).collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    for git_dir in names {
        if !is_dir_safe(&git_dir, home) {
            continue;
        }
        let pointer = match read_to_string_safe(&git_dir.join("gitdir"), home) {
            Ok(p) => PathBuf::from(p.trim()),
            Err(_) => {
                listing.stale += 1;
                continue;
            }
        };
        let Some(wt) = pointer.parent().map(Path::to_path_buf) else {
            listing.stale += 1;
            continue;
        };
        // Resolve before any stat: a stale pointer into ~/Desktop, or a symlink
        // that leads there, is counted stale and never touched.
        let Ok(wt) = resolve_safe(&wt, home) else {
            listing.stale += 1;
            continue;
        };
        if !wt.is_dir() || resolve_safe(&pointer, home).is_err() || !seen.insert(wt.clone()) {
            listing.stale += 1;
            continue;
        }
        listing.views.push(ViewId {
            worktree_path: wt,
            git_dir,
            is_main: false,
        });
    }
    Ok(listing)
}

fn open_repo(view: &ViewId) -> Result<gix::Repository> {
    gix::open_opts(&view.worktree_path, isolated_open_options())
        .with_context(|| format!("open {}", view.worktree_path.display()))
}

fn file_stat(path: &Path) -> Option<(u32, u32, u64)> {
    use std::os::unix::fs::MetadataExt;
    let (_, meta) = metadata_safe(path, home_dir().as_deref()).ok()?;
    Some((meta.mtime() as u32, meta.mtime_nsec() as u32, meta.len()))
}

/// Stat of a view's index file as `(mtime_ns, size)`; `(None, None)` when the
/// view has no index file yet.
pub fn index_stat(view: &ViewId) -> (Option<i64>, Option<i64>) {
    match file_stat(&view.git_dir.join("index")) {
        Some((s, ns, len)) => (Some(s as i64 * 1_000_000_000 + ns as i64), Some(len as i64)),
        None => (None, None),
    }
}

/// HEAD commit and branch of a view.
pub fn head_of(view: &ViewId) -> Result<(Option<String>, Option<String>)> {
    let repo = open_repo(view)?;
    let branch = repo
        .head_name()
        .ok()
        .flatten()
        .map(|n| n.shorten().to_string());
    let head = repo.head_id().ok().map(|id| id.to_string());
    Ok((branch, head))
}

/// HEAD plus index-file stat of a view. Cheap: no index parse.
pub fn view_state(view: &ViewId) -> Result<ViewState> {
    let (branch, head_oid) = head_of(view)?;
    let (index_mtime_ns, index_size) = index_stat(view);
    Ok(ViewState {
        branch,
        head_oid,
        index_mtime_ns,
        index_size,
    })
}

impl LiveIndex {
    /// Parse the view's index.
    pub fn read(view: &ViewId) -> Result<Self> {
        let repo = open_repo(view)?;
        let mtime = file_stat(&view.git_dir.join("index")).map(|(s, ns, _)| (s, ns));
        let file = repo
            .open_index()
            .with_context(|| format!("read index of {}", view.worktree_path.display()))?;
        Ok(Self { file, mtime })
    }

    /// Hex oid the index holds for a path (stage 0, or the first conflict stage).
    pub fn oid_of(&self, path: &str) -> Option<String> {
        use gix::bstr::ByteSlice;
        self.file
            .entry_by_path(path.as_bytes().as_bstr())
            .map(|e| e.id.to_string())
    }

    /// Every regular-file entry whose path `keep` accepts, in index (path)
    /// order, one per path. Symlinks, gitlinks (submodules) and sparse-checkout
    /// directory markers are not files and are skipped, as are entries marked
    /// skip-worktree (they are not on disk).
    pub fn regular_files(&self, keep: impl Fn(&str) -> bool) -> Vec<IndexEntry> {
        use gix::index::entry::{Flags, Mode};
        let mut out: Vec<IndexEntry> = Vec::new();
        for e in self.file.entries() {
            if e.mode != Mode::FILE && e.mode != Mode::FILE_EXECUTABLE {
                continue;
            }
            if e.flags.contains(Flags::SKIP_WORKTREE) {
                continue;
            }
            let path = match std::str::from_utf8(e.path(&self.file)) {
                Ok(p) => p,
                Err(_) => continue,
            };
            if !keep(path) {
                continue;
            }
            if out.last().is_some_and(|last| last.path == path) {
                continue;
            }
            out.push(IndexEntry {
                path: path.to_string(),
                oid: e.id.to_string(),
                mtime_secs: e.stat.mtime.secs,
                mtime_nsecs: e.stat.mtime.nsecs,
                size: e.stat.size,
            });
        }
        out
    }
}

/// Git blob oid of `bytes`, hex: SHA-1 over `"blob <len>\0" + bytes`, the same
/// value `git hash-object` prints.
pub fn git_blob_oid(bytes: &[u8]) -> String {
    gix::objs::compute_hash(gix::hash::Kind::Sha1, gix::objs::Kind::Blob, bytes)
        .map(|id| id.to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tcc_prefix_is_refused_lexically_without_touching_the_filesystem() {
        // None of these exist; a lexical check must answer without a stat.
        for p in [
            "/Users/nobody-here/Desktop",
            "/Users/nobody-here/Desktop/proj/src",
            "/Users/nobody-here/Documents/x",
            "/Users/nobody-here/Downloads/x/y",
            "/Users/nobody-here/desktop/x",
            "/Users/nobody-here/projects/../Desktop/x",
        ] {
            assert!(is_tcc_path(Path::new(p), None), "{p}");
        }
        for p in [
            "/Users/nobody-here/projects/Desktop",
            "/Users/nobody-here/projects/x",
            "/Users/nobody-here/DesktopStuff/x",
            "/Users/nobody-here",
        ] {
            assert!(!is_tcc_path(Path::new(p), None), "{p}");
        }
        assert!(is_tcc_path(
            Path::new("/home/z/Documents/x"),
            Some(Path::new("/home/z"))
        ));
        assert!(!is_tcc_path(
            Path::new("/home/z/projects/x"),
            Some(Path::new("/home/z"))
        ));
    }

    #[test]
    fn volumes_icloud_drive_and_the_data_firmlink_are_protected_too() {
        let home = Some(Path::new("/home/z"));
        for p in [
            "/Volumes/Backup",
            "/Volumes/Backup/proj/src/main.rs",
            "/VOLUMES/backup/x",
            "/volumes/Net Share/x",
            "/Users/nobody-here/Library/Mobile Documents",
            "/Users/nobody-here/Library/Mobile Documents/com~apple~CloudDocs/p",
            "/Users/nobody-here/library/mobile documents/x",
            "/USERS/Nobody-Here/LIBRARY/MOBILE DOCUMENTS/x",
            "/System/Volumes/Data/Users/nobody-here/Desktop",
            "/System/Volumes/Data/Users/nobody-here/Documents/p",
            "/System/Volumes/Data/Users/nobody-here/Downloads/p/q",
            "/system/volumes/data/users/nobody-here/DESKTOP/x",
            "/System/Volumes/Data/Users/nobody-here/Library/Mobile Documents/x",
            "/System/Volumes/Data/Volumes/Backup/x",
            "/Users/nobody-here/projects/../Library/Mobile Documents/x",
        ] {
            assert!(is_tcc_path(Path::new(p), None), "{p}");
        }
        // Under an injected (non-/Users) home.
        for p in [
            "/home/z/Library/Mobile Documents/x",
            "/home/z/library/MOBILE DOCUMENTS",
            "/System/Volumes/Data/home/z/Documents/x",
        ] {
            assert!(is_tcc_path(Path::new(p), home), "{p}");
        }
        assert!(is_tcc_path(
            Path::new("/home/z/Documents/x"),
            Some(Path::new("/System/Volumes/Data/home/z"))
        ));
        for p in [
            "/Volumes",
            "/Volumes/",
            "/VolumesX/a",
            "/Users/nobody-here/Library",
            "/Users/nobody-here/Library/Other",
            "/Users/nobody-here/Library/Mobile",
            "/Users/nobody-here/Library/Mobile Documents2/x",
            "/Users/nobody-here/projects/Library/Mobile Documents/x",
            "/System/Volumes/Data/Users/nobody-here/projects",
            "/System/Volumes/Data/Volumes",
            "/System/Volumes/Preboot/x",
            "/System/Library/x",
        ] {
            assert!(!is_tcc_path(Path::new(p), None), "{p}");
        }
        for p in ["/home/z/Library", "/home/z/Library/Other", "/home/z"] {
            assert!(!is_tcc_path(Path::new(p), home), "{p}");
        }
    }

    #[test]
    fn the_protected_root_is_named_in_original_case_with_a_folded_key() {
        let r = tcc_root(Path::new("/Users/Nobody/Documents/p/q"), None).unwrap();
        assert_eq!(r.shown, Path::new("/Users/Nobody/Documents"));
        assert_eq!(r.key, "/users/nobody/documents");
        let r = tcc_root(
            Path::new("/System/Volumes/Data/Users/Nobody/DESKTOP/p"),
            None,
        )
        .unwrap();
        assert_eq!(
            r.shown,
            Path::new("/System/Volumes/Data/Users/Nobody/DESKTOP")
        );
        assert_eq!(r.key, "/users/nobody/desktop", "firmlink and plain agree");
        let r = tcc_root(Path::new("/Volumes/Backup/a/b"), None).unwrap();
        assert_eq!(r.shown, Path::new("/Volumes/Backup"));
        assert_eq!(r.key, "/volumes/backup");
        let r = tcc_root(Path::new("/Users/u/Library/Mobile Documents/x"), None).unwrap();
        assert_eq!(r.shown, Path::new("/Users/u/Library/Mobile Documents"));
        assert_eq!(r.key, "/users/u/library/mobile documents");
        assert_eq!(tcc_root(Path::new("/Users/u/projects"), None), None);
        let err = identify_with(Path::new("/Volumes/Backup/repo/src"), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("/Volumes/Backup"), "{err}");
        assert!(err.contains("macOS-protected"), "{err}");
    }

    #[test]
    fn identify_refuses_a_protected_folder_before_any_filesystem_call() {
        let err = identify_with(
            Path::new("/Users/nobody-here/Desktop/repo/src/main.rs"),
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("macOS-protected"), "{err}");
    }

    #[test]
    fn temp_roots_are_recognised_lexically() {
        assert!(is_temp_path(Path::new("/private/tmp/x")));
        assert!(is_temp_path(Path::new("/tmp/x")));
        assert!(is_temp_path(Path::new("/var/folders/ab/cd/T/x")));
        assert!(is_temp_path(Path::new("/private/var/folders/ab")));
        assert!(!is_temp_path(Path::new("/Users/z/tmp/x")));
        assert!(!is_temp_path(Path::new("/tmpfoo/x")));
    }

    #[test]
    fn blob_oid_matches_known_git_values() {
        // `printf '' | git hash-object --stdin` and `printf 'hello\n' | ...`
        assert_eq!(
            git_blob_oid(b""),
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
        );
        assert_eq!(
            git_blob_oid(b"hello\n"),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
    }

    // ---- resolve_safe: fake home in a tempdir, never the real ~/Desktop -------------

    use crate::codemap::testutil::{py, write, Fx};
    use std::os::unix::fs::symlink;

    #[test]
    fn a_symlink_chain_into_a_protected_folder_is_refused_without_a_stat() {
        let fx = Fx::new();
        // The target does NOT exist: if resolve_safe lstat'ed it the error would
        // be "not found", not "protected". It must refuse first.
        let target = fx.home.join("Desktop").join("x");
        let link2 = fx.root.join("link2");
        let link1 = fx.root.join("link1");
        symlink(&target, &link2).unwrap();
        symlink(&link2, &link1).unwrap();
        let err = resolve_safe(&link1, Some(&fx.home)).unwrap_err();
        assert!(is_protected_error(&err), "{err:#}");
        let err = resolve_safe(&link1.join("deeper").join("a.rs"), Some(&fx.home)).unwrap_err();
        assert!(is_protected_error(&err), "{err:#}");
        // Same answer when the protected target does exist.
        std::fs::create_dir_all(&target).unwrap();
        let err = resolve_safe(&link1, Some(&fx.home)).unwrap_err();
        assert!(is_protected_error(&err), "{err:#}");
        // And the lexical-only spelling, which never reaches the filesystem.
        let err = resolve_safe(&target, Some(&fx.home)).unwrap_err();
        assert!(is_protected_error(&err), "{err:#}");
        // The generic /Users/<u>/Desktop spelling needs no injected home.
        let err = resolve_safe(Path::new("/Users/nobody-here/Desktop/x"), None).unwrap_err();
        assert!(is_protected_error(&err), "{err:#}");
    }

    #[test]
    fn a_symlink_chain_into_icloud_drive_or_a_volume_is_refused_without_a_stat() {
        let fx = Fx::new();
        // The parent `Library` exists (it is an ordinary folder); the protected
        // `Mobile Documents` below it does not.
        std::fs::create_dir_all(fx.home.join("Library")).unwrap();
        // Neither target exists. If resolve_safe lstat'ed it the error would be
        // "not found"; it must refuse on the lexical prefix first. Nothing here
        // creates anything under the real /Volumes.
        let icloud = fx.home.join("Library").join("Mobile Documents").join("x");
        let vol = PathBuf::from("/Volumes/csr-rs4-no-such-volume/x");
        for (name, target, root) in [
            (
                "icloud",
                icloud,
                fx.home.join("Library").join("Mobile Documents"),
            ),
            (
                "volume",
                vol,
                PathBuf::from("/Volumes/csr-rs4-no-such-volume"),
            ),
        ] {
            let link2 = fx.root.join(format!("{name}2"));
            let link1 = fx.root.join(format!("{name}1"));
            symlink(&target, &link2).unwrap();
            symlink(&link2, &link1).unwrap();
            let err = resolve_safe(&link1, Some(&fx.home)).unwrap_err();
            assert!(is_protected_error(&err), "{name}: {err:#}");
            let msg = format!("{err:#}");
            assert!(msg.contains(&root.display().to_string()), "{name}: {msg}");
            let err = resolve_safe(&link1.join("deeper").join("a.rs"), Some(&fx.home)).unwrap_err();
            assert!(is_protected_error(&err), "{name}: {err:#}");
        }
        assert!(
            !fx.home.join("Library").join("Mobile Documents").exists(),
            "nothing was created"
        );
    }

    #[test]
    fn a_chain_that_ends_outside_resolves_like_canonicalize() {
        let fx = Fx::new();
        let real = fx.root.join("real").join("dir");
        std::fs::create_dir_all(&real).unwrap();
        let link2 = fx.root.join("link2");
        let link1 = fx.root.join("link1");
        symlink(&real, &link2).unwrap();
        symlink(&link2, &link1).unwrap();
        let got = resolve_safe(&link1, Some(&fx.home)).unwrap();
        assert_eq!(got, std::fs::canonicalize(&link1).unwrap());
        assert_eq!(got, real);
        // A file below the chain, and a path that is already real.
        write(&real, "a.rs", "fn a() {}\n");
        assert_eq!(
            resolve_safe(&link1.join("a.rs"), Some(&fx.home)).unwrap(),
            real.join("a.rs")
        );
        assert_eq!(resolve_safe(&real, Some(&fx.home)).unwrap(), real);
        // A relative path is anchored at the current directory, like canonicalize.
        assert_eq!(
            resolve_safe(Path::new("."), None).unwrap(),
            std::fs::canonicalize(".").unwrap()
        );
    }

    #[test]
    fn a_symlink_loop_and_more_than_forty_hops_are_refused() {
        let fx = Fx::new();
        let a = fx.root.join("a");
        let b = fx.root.join("b");
        symlink(&b, &a).unwrap();
        symlink(&a, &b).unwrap();
        let err = resolve_safe(&a, Some(&fx.home)).unwrap_err();
        assert!(!is_protected_error(&err));
        assert!(format!("{err:#}").contains("symbolic links"), "{err:#}");

        // A chain l1 -> l2 -> ... -> lN -> dir. Exactly 40 links resolve; 41 do not.
        let chain = |n: usize, tag: &str| -> PathBuf {
            let dir = fx.root.join(format!("end-{tag}"));
            std::fs::create_dir_all(&dir).unwrap();
            let name = |i: usize| fx.root.join(format!("{tag}-{i}"));
            symlink(&dir, name(n)).unwrap();
            for i in (1..n).rev() {
                symlink(name(i + 1), name(i)).unwrap();
            }
            name(1)
        };
        let ok = chain(MAX_SYMLINK_HOPS, "ok");
        assert_eq!(
            resolve_safe(&ok, Some(&fx.home)).unwrap(),
            fx.root.join("end-ok")
        );
        let too_long = chain(MAX_SYMLINK_HOPS + 1, "long");
        let err = resolve_safe(&too_long, Some(&fx.home)).unwrap_err();
        assert!(format!("{err:#}").contains("symbolic links"), "{err:#}");
    }

    #[test]
    fn relative_targets_and_dot_dot_resolve_against_the_resolved_prefix() {
        let fx = Fx::new();
        let r = &fx.root;
        std::fs::create_dir_all(r.join("d1/real/inner")).unwrap();
        std::fs::create_dir_all(r.join("d2")).unwrap();
        // d1/link -> real (relative); d2/up -> ../d1/real; l3 -> d1/real/inner/../inner
        symlink("real", r.join("d1/link")).unwrap();
        symlink("../d1/real", r.join("d2/up")).unwrap();
        symlink("d1/real/inner/../inner", r.join("l3")).unwrap();
        let h = Some(fx.home.as_path());
        for (given, want) in [
            (r.join("d1/link"), r.join("d1/real")),
            (r.join("d2/up/inner"), r.join("d1/real/inner")),
            (r.join("l3"), r.join("d1/real/inner")),
            // `..` after a symlink pops the RESOLVED prefix (d1/real -> d1), as
            // canonicalize does; a lexical collapse would give d1.
            (r.join("d2/up/.."), r.join("d1")),
            (r.join("d1/link/inner/../.."), r.join("d1")),
        ] {
            let got = resolve_safe(&given, h).unwrap();
            assert_eq!(got, want, "{}", given.display());
            assert_eq!(got, std::fs::canonicalize(&given).unwrap());
        }
    }

    #[test]
    fn a_missing_component_is_an_error_not_a_protected_refusal() {
        let fx = Fx::new();
        let err = resolve_safe(&fx.root.join("nope").join("x"), Some(&fx.home)).unwrap_err();
        assert!(!is_protected_error(&err), "{err:#}");
        // A dangling link is missing too.
        symlink(fx.root.join("absent"), fx.root.join("dangling")).unwrap();
        let err = resolve_safe(&fx.root.join("dangling"), Some(&fx.home)).unwrap_err();
        assert!(!is_protected_error(&err), "{err:#}");
        assert!(!is_dir_safe(&fx.root.join("dangling"), Some(&fx.home)));
    }

    #[test]
    fn identify_refuses_a_symlink_into_a_protected_folder() {
        let fx = Fx::new();
        let target = fx.home.join("Documents").join("proj");
        symlink(&target, fx.root.join("lk")).unwrap();
        let err = identify_with(&fx.root.join("lk").join("src"), Some(&fx.home)).unwrap_err();
        assert!(is_protected_error(&err), "{err:#}");
    }

    #[test]
    fn a_linked_worktree_pointer_that_is_a_symlink_into_a_protected_folder_is_stale() {
        let fx = Fx::new();
        let main = fx.init_repo("a");
        write(&main, "a.py", &py(&["alpha"]));
        fx.commit_all(&main, "init");
        let wt = fx.add_worktree(&main, &fx.root.join("a-wt"), "feat");
        let ok = list_views_with(&main.join(".git"), Some(&fx.home)).unwrap();
        assert_eq!((ok.views.len(), ok.stale), (2, 0));

        // The worktree "moved" to a path that is a symlink into the fake Desktop.
        // The Desktop target is absent on purpose: nothing may stat it.
        std::fs::remove_dir_all(&wt).unwrap();
        let ptr = fx.root.join("ptr");
        symlink(fx.home.join("Desktop").join("proj"), &ptr).unwrap();
        let admin = main.join(".git/worktrees");
        let name = std::fs::read_dir(&admin)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        std::fs::write(name.join("gitdir"), format!("{}/.git\n", ptr.display())).unwrap();

        let listing = list_views_with(&main.join(".git"), Some(&fx.home)).unwrap();
        assert_eq!(listing.stale, 1, "counted stale");
        assert_eq!(listing.views.len(), 1, "and not listed");
        assert_eq!(listing.views[0].worktree_path, main);
    }

    #[test]
    fn a_submodule_style_git_dir_is_checked_before_its_worktree_is_opened() {
        let fx = Fx::new();
        let home = Some(fx.home.as_path());
        // core.worktree names a protected folder (absolute, then via a symlink).
        let gd = fx.root.join("modules").join("sub");
        std::fs::create_dir_all(&gd).unwrap();
        let desk = fx.home.join("Desktop").join("sub");
        std::fs::write(
            gd.join("config"),
            format!("[core]\n\tbare = false\n\tworktree = {}\n", desk.display()),
        )
        .unwrap();
        let listing = list_views_with(&gd, home).unwrap();
        assert_eq!((listing.views.len(), listing.stale), (0, 1));
        symlink(&desk, fx.root.join("deskl")).unwrap();
        std::fs::write(
            gd.join("config"),
            format!("[core]\nworktree = {}\n", fx.root.join("deskl").display()),
        )
        .unwrap();
        let listing = list_views_with(&gd, home).unwrap();
        assert_eq!((listing.views.len(), listing.stale), (0, 1));

        // A relative worktree outside protected folders is listed as the main view.
        let real = fx.root.join("checkout");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(
            gd.join("config"),
            "[core]\n\tworktree = ../../checkout # the work tree\n",
        )
        .unwrap();
        let listing = list_views_with(&gd, home).unwrap();
        assert_eq!((listing.views.len(), listing.stale), (1, 0));
        assert_eq!(listing.views[0].worktree_path, real);
        assert!(listing.views[0].is_main);

        // No core.worktree, or core.bare = true: no main view, nothing stale.
        std::fs::write(
            gd.join("config"),
            "[core]\n\tbare = true\n\tworktree = ../../checkout\n",
        )
        .unwrap();
        let listing = list_views_with(&gd, home).unwrap();
        assert_eq!((listing.views.len(), listing.stale), (0, 0));
        std::fs::write(gd.join("config"), "[remote \"o\"]\n\tworktree = /x\n").unwrap();
        let listing = list_views_with(&gd, home).unwrap();
        assert_eq!((listing.views.len(), listing.stale), (0, 0));
    }
}
