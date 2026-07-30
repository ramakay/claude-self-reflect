//! Canonicalize paths so linked git worktrees map onto the main worktree.
//!
//! Hook-written code-graph nodes must share a stable path key with the main
//! checkout. When Claude Code edits a file inside a linked worktree (`.git` is
//! a file pointing at `gitdir: .../.git/worktrees/<name>`), we rewrite the path
//! onto the main repo root so the same file is not stored twice.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Cache: parent directory of the input path → worktree rewrite info.
///
/// - `None` — plain repo (or no `.git`); no worktree path rewrite needed.
/// - `Some((worktree_root, main_repo_root))` — linked worktree; rewrite relative
///   suffix from worktree root onto main repo root.
type WorktreeRewrite = Option<(PathBuf, PathBuf)>;
type RewriteCache = Mutex<HashMap<PathBuf, WorktreeRewrite>>;

static CACHE: OnceLock<RewriteCache> = OnceLock::new();

fn cache() -> &'static RewriteCache {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Map a filesystem path to its canonical main-repo equivalent.
///
/// Never panics. On any parse/IO failure, returns the original `path` unchanged.
pub fn canonical_repo_path(path: &Path) -> PathBuf {
    let Some(parent) = path.parent() else {
        return path.to_path_buf();
    };
    let parent_key = parent.to_path_buf();

    // Look up cached repo-root info for this directory (no ancestor walk).
    let cached = cache()
        .lock()
        .ok()
        .and_then(|guard| guard.get(&parent_key).cloned());

    match cached {
        Some(None) => {
            // Plain repo / no rewrite — canonicalize if possible.
            return std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        }
        Some(Some((worktree_root, main_repo_root))) => {
            return rewrite_worktree_path(path, &worktree_root, &main_repo_root);
        }
        None => {}
    }

    // Cache miss: walk ancestors looking for `.git`.
    let mut dir = parent_key.clone();
    loop {
        let git_entry = dir.join(".git");
        if git_entry.exists() {
            if git_entry.is_dir() {
                // Normal (main) worktree.
                if let Ok(mut guard) = cache().lock() {
                    guard.insert(parent_key, None);
                }
                return std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
            }
            if git_entry.is_file() {
                // Linked worktree: parse gitdir and recover main repo root.
                if let Some(main_repo_root) = parse_worktree_main_root(&git_entry) {
                    let worktree_root = dir.clone();
                    if let Ok(mut guard) = cache().lock() {
                        guard.insert(
                            parent_key,
                            Some((worktree_root.clone(), main_repo_root.clone())),
                        );
                    }
                    return rewrite_worktree_path(path, &worktree_root, &main_repo_root);
                }
                // Malformed gitdir — leave path alone, do not poison the cache.
                return path.to_path_buf();
            }
        }

        match dir.parent() {
            Some(p) if p != dir.as_path() => dir = p.to_path_buf(),
            _ => break,
        }
    }

    // No `.git` found — cache as plain so we don't re-walk, return input as-is.
    if let Ok(mut guard) = cache().lock() {
        guard.insert(parent_key, None);
    }
    path.to_path_buf()
}

/// Rewrite `path` from a linked worktree onto the main repo root.
fn rewrite_worktree_path(path: &Path, worktree_root: &Path, main_repo_root: &Path) -> PathBuf {
    let Ok(rel) = path.strip_prefix(worktree_root) else {
        return path.to_path_buf();
    };
    let candidate = main_repo_root.join(rel);
    if candidate.exists() {
        std::fs::canonicalize(&candidate).unwrap_or(candidate)
    } else {
        path.to_path_buf()
    }
}

/// Read a worktree `.git` file and recover the main repository root.
///
/// Expected content: `gitdir: /abs/path/to/main/.git/worktrees/<name>\n`
fn parse_worktree_main_root(git_file: &Path) -> Option<PathBuf> {
    let contents = std::fs::read_to_string(git_file).ok()?;
    let line = contents.lines().next()?.trim();
    let gitdir = line.strip_prefix("gitdir:")?.trim();
    if gitdir.is_empty() {
        return None;
    }
    // Strip trailing `/.git/worktrees/<name>` to recover the main repo root.
    let marker = "/.git/worktrees/";
    let idx = gitdir.find(marker)?;
    let main_root = &gitdir[..idx];
    if main_root.is_empty() {
        return None;
    }
    Some(PathBuf::from(main_root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    fn canon_eq(a: &Path, b: &Path) -> bool {
        let ca = fs::canonicalize(a).unwrap_or_else(|_| a.to_path_buf());
        let cb = fs::canonicalize(b).unwrap_or_else(|_| b.to_path_buf());
        ca == cb
    }

    #[test]
    fn worktree_maps_to_main_when_target_exists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let main = tmp.path().join("main");
        let wt = tmp.path().join("wt");

        fs::create_dir_all(main.join("src")).unwrap();
        fs::create_dir_all(main.join(".git").join("worktrees").join("wt")).unwrap();
        fs::create_dir_all(wt.join("src")).unwrap();

        // Main-repo file that the worktree path should resolve to.
        let main_file = main.join("src").join("a.rs");
        fs::write(&main_file, b"fn main() {}").unwrap();

        // Linked worktree: `.git` is a file, not a directory.
        let gitdir_line = format!("gitdir: {}/.git/worktrees/wt\n", main.display());
        let mut git_file = fs::File::create(wt.join(".git")).unwrap();
        git_file.write_all(gitdir_line.as_bytes()).unwrap();

        let wt_path = wt.join("src").join("a.rs");
        // The worktree-side file need not exist — only the main-side target.
        let got = canonical_repo_path(&wt_path);
        assert!(
            canon_eq(&got, &main_file),
            "expected {:?}, got {:?}",
            main_file,
            got
        );

        // Negative: no corresponding file under main → return input unchanged.
        let missing = wt.join("src").join("does_not_exist.rs");
        let got_missing = canonical_repo_path(&missing);
        assert_eq!(got_missing, missing);
    }

    #[test]
    fn plain_git_dir_returns_same_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::create_dir_all(repo.join("src")).unwrap();
        let file = repo.join("src").join("b.rs");
        fs::write(&file, b"fn b() {}").unwrap();

        let got = canonical_repo_path(&file);
        assert!(canon_eq(&got, &file), "expected {:?}, got {:?}", file, got);
    }
}
