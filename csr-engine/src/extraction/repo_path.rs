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
            return canonicalize_stable(path);
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
                return canonicalize_stable(path);
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

    // No `.git` found. Before giving up, try the disk-free rewrite: a
    // Claude Code worktree is created at `<repo>/.claude/worktrees/<name>/`,
    // so its layout alone identifies the main checkout even after the
    // worktree (and its `.git` file) is gone. This is the common case, not a
    // corner: the graph is written from conversation history, so ingest
    // routinely runs after a worktree was removed or `git worktree prune`d,
    // and the ancestor walk above then finds nothing. Without this fallback
    // those nodes keep a dead worktree path forever and `csr_code_graph`
    // answers with files that no longer exist.
    if let Some(rewritten) = rewrite_managed_worktree_path(path) {
        // Deliberately not cached: the key is the parent directory, and a
        // resurrected worktree must go back through the `.git` path above.
        return rewritten;
    }

    // No `.git` found — cache as plain so we don't re-walk, return input as-is.
    if let Ok(mut guard) = cache().lock() {
        guard.insert(parent_key, None);
    }
    path.to_path_buf()
}

/// Path segments marking a Claude Code managed worktree: everything from
/// `.claude/worktrees/<name>/` onward is worktree-local, and the prefix
/// before it is the main repo root.
const MANAGED_WORKTREE_MARKER: [&str; 2] = [".claude", "worktrees"];

/// Rewrite `<repo>/.claude/worktrees/<name>/<rest>` to `<repo>/<rest>` using
/// path structure only — no filesystem access, so it still works once the
/// worktree is deleted.
///
/// Returns `None` when the path is not inside a managed worktree, or when
/// nothing follows the worktree name (the worktree root itself has no
/// main-repo counterpart worth naming).
fn rewrite_managed_worktree_path(path: &Path) -> Option<PathBuf> {
    let components: Vec<_> = path.components().collect();
    let marker = components.windows(2).position(|pair| {
        pair[0].as_os_str() == MANAGED_WORKTREE_MARKER[0]
            && pair[1].as_os_str() == MANAGED_WORKTREE_MARKER[1]
    })?;

    // marker, marker+1 are `.claude`, `worktrees`; marker+2 is the worktree
    // name; the remainder is the repo-relative path.
    let rest = components.get(marker + 3..)?;
    if rest.is_empty() {
        return None;
    }

    let mut out: PathBuf = components.get(..marker)?.iter().collect();
    if out.as_os_str().is_empty() {
        return None;
    }
    out.extend(rest);
    Some(out)
}

/// Canonicalize with a stable fallback for missing files (CodeRabbit PR
/// #279): `fs::canonicalize` fails once the file is deleted or moved, and a
/// raw-path fallback would make the SAME logical file yield two different
/// key strings over time (symlink-resolved while it exists, raw after) —
/// `code_nodes.file` / `code_evolution.file_path` are keyed on this string,
/// so the attribution join would silently miss. Resolving the (still
/// existing) parent directory and re-appending the file name keeps the
/// resolved spelling even after deletion.
fn canonicalize_stable(path: &Path) -> PathBuf {
    if let Ok(resolved) = std::fs::canonicalize(path) {
        return resolved;
    }
    // Walk up to the deepest still-existing ancestor (a deleted file may sit
    // under a deleted directory), canonicalize that, then re-append the
    // removed components in order.
    let mut removed: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = path;
    while let (Some(parent), Some(name)) = (cur.parent(), cur.file_name()) {
        removed.push(name.to_os_string());
        if let Ok(resolved_parent) = std::fs::canonicalize(parent) {
            let mut out = resolved_parent;
            for component in removed.iter().rev() {
                out.push(component);
            }
            return out;
        }
        cur = parent;
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
    fn deleted_file_keeps_resolved_path_key() {
        // CodeRabbit PR #279: the key for a logical file must not change
        // spelling when the file is deleted — live writers (hook) and replay
        // writers (backfill) may observe it at different times.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::create_dir_all(repo.join("src")).unwrap();
        let file = repo.join("src").join("c.rs");
        fs::write(&file, b"fn c() {}").unwrap();

        let live = canonical_repo_path(&file);
        fs::remove_file(&file).unwrap();
        let gone = canonical_repo_path(&file);
        assert_eq!(live, gone, "path key must be stable across deletion");
    }

    #[test]
    fn deleted_nested_directory_keeps_resolved_path_key() {
        // CodeRabbit PR #279 round 2: a deleted file under a DELETED
        // directory must also keep its resolved spelling — the walk must
        // find the deepest existing ancestor, not give up at the parent.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::create_dir_all(repo.join("src").join("nested")).unwrap();
        let file = repo.join("src").join("nested").join("d.rs");
        fs::write(&file, b"fn d() {}").unwrap();

        let live = canonical_repo_path(&file);
        fs::remove_dir_all(repo.join("src")).unwrap();
        let gone = canonical_repo_path(&file);
        assert_eq!(
            live, gone,
            "path key must survive deletion of intermediate directories"
        );
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

    // ---- disk-free managed-worktree rewrite ----------------------------
    //
    // Found in the live database: 384 of 9,759 `code_nodes` rows carried
    // `.../.claude/worktrees/<name>/...` paths, all written after the
    // canonicalizer shipped. The ancestor walk needs the worktree's `.git`
    // file, and ingest runs from conversation history — typically after the
    // worktree was removed or pruned — so it fell through and stored the
    // dead path.

    #[test]
    fn deleted_managed_worktree_path_still_maps_onto_the_main_repo() {
        let path = Path::new(
            "/Users/dev/projects/thing/.claude/worktrees/recap-lane/csr-engine/src/hooks/recap.rs",
        );
        assert_eq!(
            rewrite_managed_worktree_path(path),
            Some(PathBuf::from(
                "/Users/dev/projects/thing/csr-engine/src/hooks/recap.rs"
            ))
        );
    }

    #[test]
    fn managed_worktree_rewrite_ignores_unrelated_paths() {
        for path in [
            "/Users/dev/projects/thing/csr-engine/src/hooks/recap.rs",
            "/Users/dev/.claude/projects/thing/session.jsonl",
            "/Users/dev/projects/thing/.claude/settings.json",
        ] {
            assert_eq!(
                rewrite_managed_worktree_path(Path::new(path)),
                None,
                "should not rewrite {path}"
            );
        }
    }

    #[test]
    fn managed_worktree_root_itself_is_not_rewritten() {
        // Nothing follows the worktree name, so there is no main-repo file to
        // name — and an empty prefix must never produce a bare relative path.
        assert_eq!(
            rewrite_managed_worktree_path(Path::new("/repo/.claude/worktrees/lane")),
            None
        );
        assert_eq!(
            rewrite_managed_worktree_path(Path::new(".claude/worktrees/lane/src/a.rs")),
            None
        );
    }

    #[test]
    fn canonical_repo_path_uses_the_fallback_once_the_worktree_is_gone() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        let worktree = repo.join(".claude").join("worktrees").join("lane");
        fs::create_dir_all(worktree.join("src")).unwrap();
        let file = worktree.join("src").join("c.rs");
        fs::write(&file, b"fn c() {}").unwrap();
        // No `.git` anywhere: the worktree's admin file is already pruned,
        // which is exactly the state ingest observes.

        let got = canonical_repo_path(&file);
        assert_eq!(got, repo.join("src").join("c.rs"));
    }
}
