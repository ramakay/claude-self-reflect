//! Which recalled conversations belong to the asking session's repository even
//! though their stored project label differs.
//!
//! `chunks.project_name` comes from the dash-encoded folder Claude Code files a
//! transcript under (`-Users-u-projects-repo-sub` becomes `repo-sub`), while
//! the hook resolves its own cwd to the repository (`repo`). A session started
//! in a subdirectory therefore recalled none of its own history.
//!
//! The rule gives a subdirectory session exactly the treatment a session
//! started at the repository root already gets, and nothing more:
//!
//! 1. `import_state` already records every transcript file of a conversation.
//!    The path component that starts with a dash is the folder Claude Code
//!    chose from the session's starting cwd.
//! 2. That folder name is lossy (every non-alphanumeric character is `-`), so
//!    it is decoded against the filesystem as it is now: the set of existing
//!    directories whose encoding equals the folder. Exactly one is an answer.
//!    None (the directory is gone) or two (`repo/sub` and a sibling `repo-sub`
//!    both exist) is no answer.
//! 3. The decoded directory must sit inside the asker's main checkout and git
//!    must report the same common directory for it as for the asker's cwd, so
//!    a nested repository or submodule is not folded into its parent.
//! 4. A conversation widens only when every transcript file recorded for its
//!    id passes. File stems are not unique (`journal.jsonl`, copied agent
//!    transcripts), and chunk ids do not say which file they came from.
//!
//! Nothing is stored and nothing is inferred from transcript content. Every
//! uncertain case falls back to the exact label match that was the only rule
//! before, so the worst outcome of a wrong "no" is today's behaviour.
//!
//! Known limit, accepted: if a sibling repository `repo-sub` is deleted and
//! `repo/sub` exists, the sibling's old sessions decode to `repo/sub`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::search::cross_project::encode_project_folder;

/// Directory listings one folder decode may spend. A real decode lists one
/// directory per path component; a hostile tree of matching prefixes stops here
/// and counts as no answer.
const MAX_DIR_READS: usize = 64;

/// Distinct folders taken as far as a `git` subprocess for one prompt. Folders
/// that do not decode, or decode outside the asker's checkout, cost none.
const MAX_FOLDERS_VIA_GIT: usize = 3;

/// The production entry point: decode from `/`, identities from `git`.
pub(crate) fn conversations_in_asker_repo(
    paths_by_conversation: &HashMap<String, Vec<String>>,
    asker_cwd: &Path,
) -> HashSet<String> {
    conversations_in_asker_repo_with(
        paths_by_conversation,
        asker_cwd,
        Path::new("/"),
        &crate::extraction::repo_root::git_common_dir,
    )
}

/// [`conversations_in_asker_repo`] with the filesystem root and the identity
/// probe supplied, so tests need neither a real `/Users` tree nor `git`.
pub(crate) fn conversations_in_asker_repo_with(
    paths_by_conversation: &HashMap<String, Vec<String>>,
    asker_cwd: &Path,
    fs_root: &Path,
    identity_of: &dyn Fn(&Path) -> Option<String>,
) -> HashSet<String> {
    // Folder -> verdict, each folder judged once however many conversations
    // share it. Conversation order is not rank order, so sort for a stable
    // choice of which folders get the git budget.
    let mut folders: Vec<&str> = Vec::new();
    let mut folder_of_path: HashMap<&str, Option<&str>> = HashMap::new();
    for paths in paths_by_conversation.values() {
        for path in paths {
            let folder = transcript_folder(path);
            folder_of_path.insert(path.as_str(), folder);
            if let Some(folder) = folder {
                if !folders.contains(&folder) {
                    folders.push(folder);
                }
            }
        }
    }
    folders.sort_unstable();

    let mut asker: Option<Option<(String, PathBuf)>> = None;
    let mut git_budget = MAX_FOLDERS_VIA_GIT;
    let mut in_repo: HashMap<&str, bool> = HashMap::new();
    for folder in folders {
        let verdict = (|| {
            let decoded = std::fs::canonicalize(decode_folder(fs_root, folder)?).ok()?;
            let (asker_identity, checkout) = asker
                .get_or_insert_with(|| asker_repository(asker_cwd, identity_of))
                .as_ref()?;
            if !decoded.starts_with(checkout) || git_budget == 0 {
                return None;
            }
            git_budget -= 1;
            (identity_of(&decoded)? == *asker_identity).then_some(())
        })()
        .is_some();
        in_repo.insert(folder, verdict);
    }

    paths_by_conversation
        .iter()
        .filter(|(_, paths)| {
            !paths.is_empty()
                && paths.iter().all(|path| {
                    folder_of_path
                        .get(path.as_str())
                        .copied()
                        .flatten()
                        .is_some_and(|folder| in_repo.get(folder).copied().unwrap_or(false))
                })
        })
        .map(|(conversation_id, _)| conversation_id.clone())
        .collect()
}

/// The asker's repository identity and its main checkout directory. `None`
/// when the cwd is in no repository, or the common directory is not the
/// ordinary `<checkout>/.git` (a bare repository has no checkout to be inside).
fn asker_repository(
    asker_cwd: &Path,
    identity_of: &dyn Fn(&Path) -> Option<String>,
) -> Option<(String, PathBuf)> {
    let identity = identity_of(asker_cwd)?;
    let common_dir = Path::new(&identity);
    if common_dir.file_name()? != ".git" {
        return None;
    }
    let checkout = common_dir.parent()?.to_path_buf();
    Some((identity, checkout))
}

/// The Claude Code folder a transcript file lives in: the first directory
/// component of its path that starts with a dash, for a main transcript and
/// for a sidechain under `<folder>/<session>/subagents/` alike. Found by shape
/// rather than by stripping the configured projects directory, because stored
/// paths and that setting are not always spelled the same way (the watcher
/// stores canonical paths). Picking a component is not the decision: the
/// folder still has to decode to a directory inside the asker's repository.
/// `None` for rows that are not transcript paths (plans and rollouts key
/// `import_state` differently).
fn transcript_folder(file_path: &str) -> Option<&str> {
    let mut components = Path::new(file_path).components().peekable();
    while let Some(component) = components.next() {
        let name = component.as_os_str().to_str()?;
        // The last component is the file itself, never the folder.
        if name.starts_with('-') && components.peek().is_some() {
            return Some(name);
        }
    }
    None
}

/// The one existing directory whose Claude Code encoding is `folder`, or
/// `None` when there is none, more than one, or the search ran out of budget.
fn decode_folder(fs_root: &Path, folder: &str) -> Option<PathBuf> {
    // The leading dash is the root slash of the absolute cwd.
    let rest = folder.strip_prefix('-')?;
    if rest.is_empty() {
        return None;
    }
    let mut found = Vec::new();
    let mut reads = 0;
    if !decode_walk(fs_root, rest, &mut found, &mut reads) {
        return None;
    }
    match found.len() {
        1 => found.pop(),
        _ => None,
    }
}

/// Depth-first over directories whose encoded name is the next piece of
/// `rest`. Returns `false` when the listing budget ran out, which makes the
/// whole decode inconclusive. Stops early once two candidates exist.
fn decode_walk(dir: &Path, rest: &str, found: &mut Vec<PathBuf>, reads: &mut usize) -> bool {
    if *reads >= MAX_DIR_READS {
        return false;
    }
    *reads += 1;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return true;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let encoded = encode_project_folder(name);
        let Some(tail) = rest.strip_prefix(encoded.as_str()) else {
            continue;
        };
        if !(tail.is_empty() || tail.starts_with('-')) {
            continue;
        }
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if tail.is_empty() {
            found.push(path);
        } else if !decode_walk(&path, &tail[1..], found, reads) {
            return false;
        }
        if found.len() >= 2 {
            return true;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mkdirs(root: &Path, dirs: &[&str]) {
        for dir in dirs {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
    }

    /// Identity stub: the nearest ancestor holding a `.git` directory, the way
    /// git answers for an ordinary checkout, a subdirectory or a nested repo.
    fn nearest_git_dir(dir: &Path) -> Option<String> {
        let mut cur = Some(dir);
        while let Some(d) = cur {
            let git = d.join(".git");
            if git.is_dir() {
                return Some(
                    std::fs::canonicalize(git)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
            cur = d.parent();
        }
        None
    }

    fn widen(root: &Path, asker: &str, rows: &[(&str, Vec<&str>)]) -> Vec<String> {
        let map: HashMap<String, Vec<String>> = rows
            .iter()
            .map(|(id, paths)| {
                (
                    id.to_string(),
                    paths.iter().map(|p| format!("/cc/projects/{p}")).collect(),
                )
            })
            .collect();
        let mut out: Vec<String> =
            conversations_in_asker_repo_with(&map, &root.join(asker), root, &nearest_git_dir)
                .into_iter()
                .collect();
        out.sort();
        out
    }

    #[test]
    fn the_folder_is_found_by_shape_wherever_the_projects_directory_is() {
        for (path, folder) in [
            (
                "/Users/u/.claude/projects/-Users-u-projects-repo-sub/s1.jsonl",
                Some("-Users-u-projects-repo-sub"),
            ),
            (
                "/private/var/cc/-Users-u-projects-repo/s1/subagents/agent-a1.jsonl",
                Some("-Users-u-projects-repo"),
            ),
            ("/Users/u/.claude/projects/-Users-u-projects-repo", None),
            ("/Users/u/.claude/projects/loose.jsonl", None),
            ("plan:some-slug", None),
            ("", None),
        ] {
            assert_eq!(transcript_folder(path), folder, "{path}");
        }
    }

    #[test]
    fn a_subdirectory_folder_decodes_to_the_one_directory_that_exists() {
        let tmp = tempfile::TempDir::new().unwrap();
        mkdirs(tmp.path(), &["Users/u/projects/repo/sub"]);
        assert_eq!(
            decode_folder(tmp.path(), "-Users-u-projects-repo-sub"),
            Some(tmp.path().join("Users/u/projects/repo/sub"))
        );
    }

    #[test]
    fn dotted_and_nested_names_decode_through_their_dashes() {
        let tmp = tempfile::TempDir::new().unwrap();
        mkdirs(tmp.path(), &["Users/u/projects/repo/.claude/worktrees/w1"]);
        assert_eq!(
            decode_folder(tmp.path(), "-Users-u-projects-repo--claude-worktrees-w1"),
            Some(
                tmp.path()
                    .join("Users/u/projects/repo/.claude/worktrees/w1")
            )
        );
    }

    #[test]
    fn a_subdirectory_and_a_sibling_with_one_encoding_is_no_answer() {
        let tmp = tempfile::TempDir::new().unwrap();
        mkdirs(
            tmp.path(),
            &["Users/u/projects/repo/sub", "Users/u/projects/repo-sub"],
        );
        assert_eq!(
            decode_folder(tmp.path(), "-Users-u-projects-repo-sub"),
            None
        );
    }

    #[test]
    fn a_folder_whose_directory_is_gone_is_no_answer() {
        let tmp = tempfile::TempDir::new().unwrap();
        mkdirs(tmp.path(), &["Users/u/projects/repo"]);
        assert_eq!(
            decode_folder(tmp.path(), "-Users-u-projects-repo-sub"),
            None
        );
        assert_eq!(decode_folder(tmp.path(), "Users-u-projects-repo"), None);
        assert_eq!(decode_folder(tmp.path(), "-"), None);
    }

    #[test]
    fn a_decode_that_runs_out_of_listings_is_no_answer() {
        let tmp = tempfile::TempDir::new().unwrap();
        // a/a/a/... deeper than the budget, with the target at the bottom.
        let depth = MAX_DIR_READS + 4;
        let path: PathBuf = ["a"].iter().cycle().take(depth).collect();
        std::fs::create_dir_all(tmp.path().join(&path)).unwrap();
        let folder = "-a".repeat(depth);
        assert_eq!(decode_folder(tmp.path(), &folder), None);
        let shallow = "-a".repeat(8);
        assert!(decode_folder(tmp.path(), &shallow).is_some());
    }

    #[test]
    fn a_subdirectory_session_widens_and_a_sibling_repository_does_not() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        mkdirs(
            &root,
            &[
                "Users/u/projects/repo/.git",
                "Users/u/projects/repo/sub",
                "Users/u/projects/repo-tools/.git",
            ],
        );
        // The tempdir stands in for `/`, so folders are encoded from it down.
        assert_eq!(
            widen(
                &root,
                "Users/u/projects/repo",
                &[
                    ("s1", vec!["-Users-u-projects-repo-sub/s1.jsonl"]),
                    (
                        "agent-a1",
                        vec!["-Users-u-projects-repo-sub/s1/subagents/agent-a1.jsonl"],
                    ),
                    ("s2", vec!["-Users-u-projects-repo-tools/s2.jsonl"]),
                ],
            ),
            ["agent-a1", "s1"]
        );
    }

    #[test]
    fn a_nested_repository_inside_the_checkout_is_not_folded_in() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        mkdirs(
            &root,
            &[
                "Users/u/projects/repo/.git",
                "Users/u/projects/repo/vendor/other/.git",
            ],
        );
        assert_eq!(
            widen(
                &root,
                "Users/u/projects/repo",
                &[("s1", vec!["-Users-u-projects-repo-vendor-other/s1.jsonl"])],
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn one_file_of_a_shared_stem_elsewhere_keeps_the_whole_id_out() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        mkdirs(
            &root,
            &[
                "Users/u/projects/repo/.git",
                "Users/u/projects/repo/sub",
                "Users/u/projects/other/.git",
            ],
        );
        assert_eq!(
            widen(
                &root,
                "Users/u/projects/repo",
                &[(
                    "journal",
                    vec![
                        "-Users-u-projects-repo-sub/w/journal.jsonl",
                        "-Users-u-projects-other/w/journal.jsonl",
                    ],
                )],
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn rows_that_name_no_folder_never_widen() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        mkdirs(
            &root,
            &["Users/u/projects/repo/.git", "Users/u/projects/repo/sub"],
        );
        let map: HashMap<String, Vec<String>> = [
            ("plan".to_string(), vec!["plan:some-slug".to_string()]),
            (
                "loose".to_string(),
                vec!["/cc/projects/loose.jsonl".to_string()],
            ),
            ("empty".to_string(), Vec::new()),
            (
                "file-named-like-a-folder".to_string(),
                vec!["/cc/projects/-Users-u-projects-repo-sub".to_string()],
            ),
        ]
        .into();
        assert!(conversations_in_asker_repo_with(
            &map,
            &root.join("Users/u/projects/repo"),
            &root,
            &nearest_git_dir,
        )
        .is_empty());
    }

    #[test]
    fn an_asker_outside_any_repository_widens_nothing_and_a_bare_one_too() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        mkdirs(&root, &["Users/u/projects/plain/sub"]);
        assert_eq!(
            widen(
                &root,
                "Users/u/projects/plain",
                &[("s1", vec!["-Users-u-projects-plain-sub/s1.jsonl"])],
            ),
            Vec::<String>::new()
        );
        let bare = |_: &Path| Some("/srv/git/repo.git".to_string());
        let map: HashMap<String, Vec<String>> = [(
            "s1".to_string(),
            vec!["/cc/projects/-Users-u-projects-plain-sub/s1.jsonl".to_string()],
        )]
        .into();
        assert!(conversations_in_asker_repo_with(
            &map,
            &root.join("Users/u/projects/plain"),
            &root,
            &bare,
        )
        .is_empty());
    }

    #[test]
    fn only_a_few_folders_reach_git_for_one_prompt() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        mkdirs(&root, &["Users/u/projects/repo/.git"]);
        let mut rows: Vec<(String, String)> = Vec::new();
        for i in 0..6 {
            mkdirs(&root, &[&format!("Users/u/projects/repo/d{i}")]);
            rows.push((
                format!("s{i}"),
                format!("/cc/projects/-Users-u-projects-repo-d{i}/s{i}.jsonl"),
            ));
        }
        let map: HashMap<String, Vec<String>> = rows
            .into_iter()
            .map(|(id, path)| (id, vec![path]))
            .collect();
        let calls = std::cell::Cell::new(0usize);
        let counting = |dir: &Path| {
            calls.set(calls.get() + 1);
            nearest_git_dir(dir)
        };
        let widened = conversations_in_asker_repo_with(
            &map,
            &root.join("Users/u/projects/repo"),
            &root,
            &counting,
        );
        assert_eq!(widened.len(), MAX_FOLDERS_VIA_GIT);
        // One probe for the asker, then one per folder inside the budget.
        assert_eq!(calls.get(), 1 + MAX_FOLDERS_VIA_GIT);
    }
}
