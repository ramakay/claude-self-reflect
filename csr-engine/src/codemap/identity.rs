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

fn lower(p: &Path) -> String {
    p.to_string_lossy().to_lowercase()
}

/// True when `path` is, or is inside, `~/Desktop`, `~/Documents` or
/// `~/Downloads` of any user. Lexical only: the filesystem is not consulted, so
/// calling this never triggers a macOS privacy prompt. Case-insensitive (APFS).
/// `home` adds the same three folders under a non-`/Users` home.
pub fn is_tcc_path(path: &Path, home: Option<&Path>) -> bool {
    let norm = normalize_lexical(path);
    let comps: Vec<String> = norm
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    // ["/", "users", "<user>", "desktop", ...]
    if comps.len() >= 4 && comps[1] == "users" && TCC_FOLDERS.contains(&comps[3].as_str()) {
        return true;
    }
    if let Some(home) = home {
        let home = normalize_lexical(home);
        let l = norm.to_string_lossy().to_lowercase();
        for folder in ["Desktop", "Documents", "Downloads"] {
            let protected = lower(&home.join(folder));
            if l == protected || l.starts_with(&format!("{protected}/")) {
                return true;
            }
        }
    }
    false
}

/// True when `path` is inside a temp root (`/private/tmp`, `/tmp`,
/// `/var/folders`, `/private/var/folders`). Lexical.
pub fn is_temp_path(path: &Path) -> bool {
    let norm = normalize_lexical(path);
    TEMP_ROOTS
        .iter()
        .any(|root| norm == Path::new(root) || norm.starts_with(root))
}

fn refuse_if_protected(path: &Path, home: Option<&Path>) -> Result<()> {
    if is_tcc_path(path, home) {
        bail!(
            "refusing {}: macOS-protected folder (Desktop, Documents, Downloads)",
            path.display()
        );
    }
    Ok(())
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
    let mut dir = abs.clone();
    while !dir.is_dir() {
        match dir.parent() {
            Some(p) => dir = p.to_path_buf(),
            None => bail!("{} does not exist", abs.display()),
        }
    }
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
        if let Ok(meta) = std::fs::metadata(&dot_git) {
            let common = if meta.is_dir() {
                dot_git.clone()
            } else {
                common_dir_from_pointer(&dir, &dot_git, home)?
            };
            let common_dir = std::fs::canonicalize(&common)
                .with_context(|| format!("resolve {}", common.display()))?;
            let worktree_root = std::fs::canonicalize(&dir)?;
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

/// Read a `.git` pointer file and return the repo's common dir.
fn common_dir_from_pointer(dir: &Path, dot_git: &Path, home: Option<&Path>) -> Result<PathBuf> {
    let text =
        std::fs::read_to_string(dot_git).with_context(|| format!("read {}", dot_git.display()))?;
    let target = text
        .lines()
        .find_map(|l| l.strip_prefix("gitdir:"))
        .map(str::trim)
        .ok_or_else(|| anyhow!("{} has no gitdir: line", dot_git.display()))?;
    let gitdir = normalize_lexical(&dir.join(target));
    refuse_if_protected(&gitdir, home)?;
    let commondir_file = gitdir.join("commondir");
    match std::fs::read_to_string(&commondir_file) {
        Ok(rel) => {
            let common = normalize_lexical(&gitdir.join(rel.trim()));
            refuse_if_protected(&common, home)?;
            Ok(common)
        }
        // A submodule's pointer names its own git dir, which is its common dir.
        Err(_) => Ok(gitdir),
    }
}

/// The git dir that holds a worktree's HEAD and index: `<root>/.git` for a
/// normal checkout, the `gitdir:` target for a linked worktree or submodule.
pub fn git_dir_of_worktree(root: &Path, home: Option<&Path>) -> Result<PathBuf> {
    refuse_if_protected(root, home)?;
    let dot_git = root.join(".git");
    let meta =
        std::fs::metadata(&dot_git).with_context(|| format!("{} has no .git", root.display()))?;
    if meta.is_dir() {
        return Ok(dot_git);
    }
    let text = std::fs::read_to_string(&dot_git)?;
    let target = text
        .lines()
        .find_map(|l| l.strip_prefix("gitdir:"))
        .map(str::trim)
        .ok_or_else(|| anyhow!("{} has no gitdir: line", dot_git.display()))?;
    let gitdir = normalize_lexical(&root.join(target));
    refuse_if_protected(&gitdir, home)?;
    Ok(gitdir)
}

fn isolated_open_options() -> gix::open::Options {
    gix::open::Options::isolated()
}

/// Where a repo's main worktree is, or `None` for a bare repo. Never touches a
/// protected folder.
fn main_worktree_path(common_dir: &Path) -> Option<PathBuf> {
    if common_dir.file_name().is_some_and(|n| n == ".git") {
        return common_dir.parent().map(Path::to_path_buf);
    }
    // Submodule git dirs and unusual layouts: ask git's config (core.worktree).
    let repo = gix::open_opts(common_dir, isolated_open_options()).ok()?;
    repo.workdir().and_then(|w| std::fs::canonicalize(w).ok())
}

/// The main worktree (unless the repo is bare) plus every linked worktree whose
/// directory still exists. Stale entries are skipped and counted.
pub fn list_views(common_dir: &Path) -> Result<ViewListing> {
    list_views_with(common_dir, home_dir().as_deref())
}

pub fn list_views_with(common_dir: &Path, home: Option<&Path>) -> Result<ViewListing> {
    refuse_if_protected(common_dir, home)?;
    let mut listing = ViewListing::default();
    let mut seen: HashSet<PathBuf> = HashSet::new();

    if let Some(main) = main_worktree_path(common_dir) {
        if is_tcc_path(&main, home) {
            listing.stale += 1;
        } else if main.is_dir() {
            let main = std::fs::canonicalize(&main).unwrap_or(main);
            seen.insert(main.clone());
            listing.views.push(ViewId {
                worktree_path: main,
                git_dir: common_dir.to_path_buf(),
                is_main: true,
            });
        }
    }

    let linked_root = common_dir.join("worktrees");
    let mut names: Vec<PathBuf> = match std::fs::read_dir(&linked_root) {
        Ok(rd) => rd.filter_map(|e| e.ok()).map(|e| e.path()).collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    for git_dir in names {
        if !git_dir.is_dir() {
            continue;
        }
        let pointer = match std::fs::read_to_string(git_dir.join("gitdir")) {
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
        // Lexical check first: a stale pointer into ~/Desktop must not be stat'ed.
        if is_tcc_path(&wt, home) || !wt.is_dir() || !pointer.exists() {
            listing.stale += 1;
            continue;
        }
        let wt = std::fs::canonicalize(&wt).unwrap_or(wt);
        if is_tcc_path(&wt, home) || !seen.insert(wt.clone()) {
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
    let meta = std::fs::metadata(path).ok()?;
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
}
