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
//! 1. `import_state` records the transcript files of a conversation. The
//!    folder is the immediate child of the configured projects directory that
//!    the file sits under; a path that is not under it names no folder.
//! 2. That folder name is lossy (every non-alphanumeric character is `-`), so
//!    it is decoded against the filesystem as it is now: the set of existing
//!    directories whose encoding equals the folder. Exactly one is an answer.
//!    None (the directory is gone), two (`repo/sub` and a sibling `repo-sub`
//!    both exist), or any filesystem error on the way is no answer.
//! 3. The decoded directory must sit inside the asker's main checkout and git
//!    must report the same common directory for it as for the asker's cwd, so
//!    a nested repository or submodule is not folded into its parent.
//! 4. A conversation widens only when every transcript file recorded for its
//!    id passes, and there are few of them. File stems are not unique
//!    (`journal.jsonl`, copied agent transcripts).
//! 5. `import_state` is written after the chunks, so it cannot prove which
//!    file a chunk came from. The chunk's own label can: it is written with
//!    the chunk. A widened conversation admits only chunks labelled after one
//!    of its passing folders, so a chunk overwritten by a same-stem file from
//!    elsewhere keeps that file's label and stays out.
//!
//! Nothing is stored and transcript content is not read. Every uncertain case
//! falls back to the exact label match that was the only rule before, so the
//! worst outcome of a wrong "no" is the old behaviour. All of it runs under one
//! budget of directory listings, `git` calls and wall-clock time.
//!
//! Known limit, accepted: the decode sees the filesystem as it is now. A
//! directory that used to be a different repository and now resolves inside
//! the asker's checkout is taken for the asker's: a deleted sibling `repo-sub`
//! when `repo/sub` exists, or a nested repository whose `.git` was removed
//! while its directory stayed.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::search::cross_project::encode_project_folder;

/// Conversation id to the labels its chunks may carry and still be recalled.
pub(crate) type RepoConversations = HashMap<String, HashSet<String>>;

/// A conversation with more transcript files than this never widens. A real
/// session has one; a stem shared across dozens of folders is not a session.
pub(crate) const MAX_PATHS_PER_CONVERSATION: usize = 8;

/// What one evaluation of the rule may spend, across all folders together.
pub(crate) struct Budget {
    dir_reads: usize,
    git_calls: usize,
    deadline: Instant,
}

impl Budget {
    /// One prompt: a real decode lists one directory per path component.
    pub(crate) fn for_prompt() -> Self {
        Self {
            dir_reads: 64,
            git_calls: 3,
            deadline: Instant::now() + Duration::from_millis(250),
        }
    }

    /// The MCP server asks about the whole corpus at once and keeps the
    /// answer (see `mcp::scope`), so it can afford more.
    pub(crate) fn for_server() -> Self {
        Self {
            dir_reads: 4096,
            git_calls: 32,
            deadline: Instant::now() + Duration::from_secs(5),
        }
    }

    fn expired(&self) -> bool {
        Instant::now() >= self.deadline
    }

    fn take_dir_read(&mut self) -> bool {
        if self.dir_reads == 0 || self.expired() {
            return false;
        }
        self.dir_reads -= 1;
        true
    }

    fn take_git_call(&mut self) -> bool {
        if self.git_calls == 0 || self.expired() {
            return false;
        }
        self.git_calls -= 1;
        true
    }
}

/// The rule for one prompt: decode from `/`, identities from `git`, and a hard
/// stop at the budget's deadline even if a filesystem call or `git` hangs.
pub(crate) fn conversations_in_asker_repo(
    paths_by_conversation: &HashMap<String, Vec<String>>,
    projects_dir: &Path,
    asker_cwd: &Path,
) -> RepoConversations {
    within_deadline(
        paths_by_conversation,
        projects_dir,
        asker_cwd,
        Budget::for_prompt(),
        || {},
    )
}

/// The same rule for the MCP server, with the server's budget. `finished` runs
/// on the worker when it is really over, which can be long after this returns:
/// the server uses it to never start a second worker beside a stuck one.
pub(crate) fn conversations_in_client_repo(
    paths_by_conversation: &HashMap<String, Vec<String>>,
    projects_dir: &Path,
    client_dir: &Path,
    finished: impl FnOnce() + Send + 'static,
) -> RepoConversations {
    within_deadline(
        paths_by_conversation,
        projects_dir,
        client_dir,
        Budget::for_server(),
        finished,
    )
}

/// Runs its closure when dropped: on return, on panic, and when the thread
/// that was meant to own it could not be started.
struct OnDrop<F: FnOnce()>(Option<F>);

impl<F: FnOnce()> Drop for OnDrop<F> {
    fn drop(&mut self) {
        if let Some(f) = self.0.take() {
            f();
        }
    }
}

/// Run the rule on its own thread and stop waiting at the deadline. The budget
/// is checked between steps, but a single `read_dir` on a dead network mount
/// cannot be interrupted; the caller must not hang with it. A late answer is
/// dropped, which widens nothing.
fn within_deadline(
    paths_by_conversation: &HashMap<String, Vec<String>>,
    projects_dir: &Path,
    asker_cwd: &Path,
    budget: Budget,
    finished: impl FnOnce() + Send + 'static,
) -> RepoConversations {
    let wait = budget.deadline.saturating_duration_since(Instant::now());
    let (paths, projects_dir, asker_cwd) = (
        paths_by_conversation.clone(),
        projects_dir.to_path_buf(),
        asker_cwd.to_path_buf(),
    );
    let (tx, rx) = std::sync::mpsc::channel();
    let finished = OnDrop(Some(finished));
    let worker = std::thread::Builder::new().spawn(move || {
        let _finished = finished;
        let _ = tx.send(conversations_in_asker_repo_with(
            &paths,
            &projects_roots(&projects_dir),
            &asker_cwd,
            Path::new("/"),
            &crate::extraction::repo_root::git_common_dir,
            budget,
        ));
    });
    if worker.is_err() {
        return RepoConversations::new();
    }
    rx.recv_timeout(wait).unwrap_or_default()
}

/// The configured projects directory as given and as the filesystem resolves
/// it. The watcher stores canonical transcript paths and the hook importer
/// stores them as handed over, so a stored path may use either spelling.
fn projects_roots(projects_dir: &Path) -> Vec<PathBuf> {
    let mut roots = vec![projects_dir.to_path_buf()];
    if let Ok(canonical) = std::fs::canonicalize(projects_dir) {
        if canonical != projects_dir {
            roots.push(canonical);
        }
    }
    roots
}

/// The rule with the filesystem root and the identity probe supplied, so tests
/// need neither a real `/Users` tree nor `git`.
pub(crate) fn conversations_in_asker_repo_with(
    paths_by_conversation: &HashMap<String, Vec<String>>,
    projects_roots: &[PathBuf],
    asker_cwd: &Path,
    fs_root: &Path,
    identity_of: &dyn Fn(&Path, Instant) -> Option<String>,
    mut budget: Budget,
) -> RepoConversations {
    // Only conversations every one of whose files names a folder can pass, so
    // only their folders are worth judging. Sorted, so which folders get the
    // budget does not depend on hash order.
    let mut candidates: Vec<(&String, Vec<&str>)> = Vec::new();
    let mut folders: BTreeSet<&str> = BTreeSet::new();
    for (conversation_id, paths) in paths_by_conversation {
        if paths.is_empty() || paths.len() > MAX_PATHS_PER_CONVERSATION {
            continue;
        }
        let named: Option<Vec<&str>> = paths
            .iter()
            .map(|path| transcript_folder(projects_roots, path))
            .collect();
        if let Some(named) = named {
            folders.extend(named.iter().copied());
            candidates.push((conversation_id, named));
        }
    }

    let mut asker: Option<Option<(String, PathBuf)>> = None;
    let mut in_repo: HashSet<&str> = HashSet::new();
    for folder in folders {
        // Once the budget is gone nothing further can pass: stop, do not decode.
        if budget.expired() || budget.git_calls == 0 {
            break;
        }
        let deadline = budget.deadline;
        let passed = (|| {
            let decoded =
                std::fs::canonicalize(decode_folder(fs_root, folder, &mut budget)?).ok()?;
            let (asker_identity, checkout) = asker
                .get_or_insert_with(|| asker_repository(asker_cwd, identity_of, deadline))
                .as_ref()?;
            if !decoded.starts_with(checkout) || !budget.take_git_call() {
                return None;
            }
            (identity_of(&decoded, deadline)? == *asker_identity).then_some(())
        })()
        .is_some();
        if passed {
            in_repo.insert(folder);
        }
    }

    candidates
        .into_iter()
        .filter(|(_, named)| named.iter().all(|folder| in_repo.contains(folder)))
        .map(|(conversation_id, named)| {
            let labels = named
                .into_iter()
                .map(crate::import::normalize_project_name)
                .collect();
            (conversation_id.clone(), labels)
        })
        .collect()
}

/// The asker's repository identity and its main checkout directory. `None`
/// when the cwd is in no repository, or the common directory is not the
/// ordinary `<checkout>/.git` (a bare repository has no checkout to be inside).
fn asker_repository(
    asker_cwd: &Path,
    identity_of: &dyn Fn(&Path, Instant) -> Option<String>,
    deadline: Instant,
) -> Option<(String, PathBuf)> {
    let identity = identity_of(asker_cwd, deadline)?;
    let common_dir = Path::new(&identity);
    if common_dir.file_name()? != ".git" {
        return None;
    }
    let checkout = common_dir.parent()?.to_path_buf();
    Some((identity, checkout))
}

/// The Claude Code folder a transcript file lives in: the immediate child of
/// the projects directory, for a main transcript and for a sidechain under
/// `<folder>/<session>/subagents/` alike. `None` for a path that is not under
/// any spelling of the projects directory (plan and rollout rows, a transcript
/// handed to the hook importer from somewhere else), for a file directly under
/// it, and for a child that is not a dash-encoded absolute path.
fn transcript_folder<'a>(projects_roots: &[PathBuf], file_path: &'a str) -> Option<&'a str> {
    let path = Path::new(file_path);
    let relative = projects_roots
        .iter()
        .find_map(|root| path.strip_prefix(root).ok())?;
    let mut components = relative.components();
    let folder = components.next()?.as_os_str().to_str()?;
    components.next()?;
    folder.starts_with('-').then_some(folder)
}

/// The one existing directory whose Claude Code encoding is `folder`, or
/// `None` when there is none, more than one, the search hit an error, or the
/// budget ran out before it finished.
fn decode_folder(fs_root: &Path, folder: &str, budget: &mut Budget) -> Option<PathBuf> {
    // The leading dash is the root slash of the absolute cwd.
    let rest = folder.strip_prefix('-')?;
    if rest.is_empty() {
        return None;
    }
    let mut found = Vec::new();
    if !decode_walk(fs_root, rest, &mut found, budget) {
        return None;
    }
    match found.len() {
        1 => found.pop(),
        _ => None,
    }
}

/// Depth-first over directories whose encoded name is the next piece of
/// `rest`. `false` means inconclusive: the budget ran out, or a listing, an
/// entry or a metadata read failed, so a branch that might hold a second
/// candidate went unseen. Stops early once two candidates exist.
fn decode_walk(dir: &Path, rest: &str, found: &mut Vec<PathBuf>, budget: &mut Budget) -> bool {
    if !budget.take_dir_read() {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries {
        let Ok(entry) = entry else {
            return false;
        };
        let name = entry.file_name();
        let encoded = encode_project_folder(&name.to_string_lossy());
        let Some(tail) = rest.strip_prefix(encoded.as_str()) else {
            continue;
        };
        if !(tail.is_empty() || tail.starts_with('-')) {
            continue;
        }
        // A name that is not valid UTF-8 was seen by Claude Code as some other
        // string; whether it matches cannot be known from here.
        if name.to_str().is_none() {
            return false;
        }
        let path = entry.path();
        match std::fs::metadata(&path) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => continue,
            Err(_) => return false,
        }
        if tail.is_empty() {
            found.push(path);
        } else if !decode_walk(&path, &tail[1..], found, budget) {
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

    fn roomy() -> Budget {
        Budget {
            dir_reads: 64,
            git_calls: 3,
            deadline: Instant::now() + Duration::from_secs(30),
        }
    }

    /// Identity stub: the nearest ancestor holding a `.git` directory, the way
    /// git answers for an ordinary checkout, a subdirectory or a nested repo.
    fn nearest_git_dir(dir: &Path, _deadline: Instant) -> Option<String> {
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

    fn rows(rows: &[(&str, Vec<&str>)]) -> HashMap<String, Vec<String>> {
        rows.iter()
            .map(|(id, paths)| {
                (
                    id.to_string(),
                    paths.iter().map(|p| format!("/cc/projects/{p}")).collect(),
                )
            })
            .collect()
    }

    /// Widened conversations as sorted `id=label,label` strings. The tempdir
    /// stands in for `/`, so folders are encoded from it down.
    fn widen(root: &Path, asker: &str, table: &[(&str, Vec<&str>)]) -> Vec<String> {
        let mut out: Vec<String> = conversations_in_asker_repo_with(
            &rows(table),
            &[PathBuf::from("/cc/projects")],
            &root.join(asker),
            root,
            &nearest_git_dir,
            roomy(),
        )
        .into_iter()
        .map(|(id, labels)| {
            let mut labels: Vec<String> = labels.into_iter().collect();
            labels.sort();
            format!("{id}={}", labels.join(","))
        })
        .collect();
        out.sort();
        out
    }

    fn decode(root: &Path, folder: &str) -> Option<PathBuf> {
        decode_folder(root, folder, &mut roomy())
    }

    #[test]
    fn the_folder_is_the_immediate_child_of_the_projects_directory_and_nothing_else() {
        let roots = [
            PathBuf::from("/Users/u/.claude/projects"),
            PathBuf::from("/private/cc"),
        ];
        for (path, folder) in [
            (
                "/Users/u/.claude/projects/-Users-u-projects-repo-sub/s1.jsonl",
                Some("-Users-u-projects-repo-sub"),
            ),
            (
                "/private/cc/-Users-u-projects-repo/s1/subagents/agent-a1.jsonl",
                Some("-Users-u-projects-repo"),
            ),
            // A dash-named directory that is not the child of a projects root.
            ("/r/-r-sub/archive/session.jsonl", None),
            (
                "/Users/u/.claude/projects/archive/-Users-u-projects-repo-sub/s1.jsonl",
                None,
            ),
            ("/Users/u/.claude/projects/-Users-u-projects-repo", None),
            ("/Users/u/.claude/projects/loose.jsonl", None),
            ("plan:some-slug", None),
            ("", None),
        ] {
            assert_eq!(transcript_folder(&roots, path), folder, "{path}");
        }
    }

    #[test]
    fn a_subdirectory_folder_decodes_to_the_one_directory_that_exists() {
        let tmp = tempfile::TempDir::new().unwrap();
        mkdirs(tmp.path(), &["Users/u/projects/repo/sub"]);
        assert_eq!(
            decode(tmp.path(), "-Users-u-projects-repo-sub"),
            Some(tmp.path().join("Users/u/projects/repo/sub"))
        );
    }

    #[test]
    fn dotted_and_nested_names_decode_through_their_dashes() {
        let tmp = tempfile::TempDir::new().unwrap();
        mkdirs(tmp.path(), &["Users/u/projects/repo/.claude/worktrees/w1"]);
        assert_eq!(
            decode(tmp.path(), "-Users-u-projects-repo--claude-worktrees-w1"),
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
        assert_eq!(decode(tmp.path(), "-Users-u-projects-repo-sub"), None);
    }

    #[test]
    fn a_folder_whose_directory_is_gone_is_no_answer() {
        let tmp = tempfile::TempDir::new().unwrap();
        mkdirs(tmp.path(), &["Users/u/projects/repo"]);
        assert_eq!(decode(tmp.path(), "-Users-u-projects-repo-sub"), None);
        assert_eq!(decode(tmp.path(), "Users-u-projects-repo"), None);
        assert_eq!(decode(tmp.path(), "-"), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_branch_that_cannot_be_listed_makes_the_decode_inconclusive() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        // `-a-b-c` is `a/b/c` or `a-b/c`. The second cannot be listed, so it
        // cannot be ruled out, so the first is not a unique answer.
        mkdirs(tmp.path(), &["a/b/c", "a-b"]);
        let locked = tmp.path().join("a-b");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let unreadable = std::fs::read_dir(&locked).is_err();
        let got = decode(tmp.path(), "-a-b-c");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        if unreadable {
            assert_eq!(got, None);
        } // running as root: the directory stayed listable, nothing to assert
        assert_eq!(decode(tmp.path(), "-a-b-c"), Some(tmp.path().join("a/b/c")));
    }

    #[test]
    fn the_listing_budget_is_shared_and_running_out_is_no_answer() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path: PathBuf = ["a"].iter().cycle().take(12).collect();
        std::fs::create_dir_all(tmp.path().join(&path)).unwrap();
        let folder = "-a".repeat(12);
        let mut budget = Budget {
            dir_reads: 20,
            ..roomy()
        };
        assert!(decode_folder(tmp.path(), &folder, &mut budget).is_some());
        // The first decode spent 12 of the 20 listings; the second cannot finish.
        assert_eq!(decode_folder(tmp.path(), &folder, &mut budget), None);
        let mut late = Budget {
            deadline: Instant::now(),
            ..roomy()
        };
        assert_eq!(decode_folder(tmp.path(), &folder, &mut late), None);
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
            ["agent-a1=repo-sub", "s1=repo-sub"]
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
    fn a_stem_with_many_files_never_widens_even_when_all_of_them_pass() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        mkdirs(
            &root,
            &["Users/u/projects/repo/.git", "Users/u/projects/repo/sub"],
        );
        let many: Vec<String> = (0..=MAX_PATHS_PER_CONVERSATION)
            .map(|i| format!("-Users-u-projects-repo-sub/w{i}/journal.jsonl"))
            .collect();
        let few = &many[..MAX_PATHS_PER_CONVERSATION];
        assert_eq!(
            widen(
                &root,
                "Users/u/projects/repo",
                &[("journal", many.iter().map(String::as_str).collect())],
            ),
            Vec::<String>::new()
        );
        assert_eq!(
            widen(
                &root,
                "Users/u/projects/repo",
                &[("journal", few.iter().map(String::as_str).collect())],
            ),
            ["journal=repo-sub"]
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
                "outside-the-projects-directory".to_string(),
                vec!["/other/-Users-u-projects-repo-sub/s.jsonl".to_string()],
            ),
            (
                "one-good-one-outside".to_string(),
                vec![
                    "/cc/projects/-Users-u-projects-repo-sub/s.jsonl".to_string(),
                    "/other/-Users-u-projects-repo-sub/s.jsonl".to_string(),
                ],
            ),
        ]
        .into();
        assert!(conversations_in_asker_repo_with(
            &map,
            &[PathBuf::from("/cc/projects")],
            &root.join("Users/u/projects/repo"),
            &root,
            &nearest_git_dir,
            roomy(),
        )
        .is_empty());
    }

    #[test]
    fn an_asker_outside_any_repository_widens_nothing_and_a_bare_one_too() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        mkdirs(&root, &["Users/u/projects/plain/sub"]);
        let table = [("s1", vec!["-Users-u-projects-plain-sub/s1.jsonl"])];
        assert_eq!(
            widen(&root, "Users/u/projects/plain", &table),
            Vec::<String>::new()
        );
        let bare = |_: &Path, _: Instant| Some("/srv/git/repo.git".to_string());
        assert!(conversations_in_asker_repo_with(
            &rows(&table),
            &[PathBuf::from("/cc/projects")],
            &root.join("Users/u/projects/plain"),
            &root,
            &bare,
            roomy(),
        )
        .is_empty());
    }

    #[test]
    fn the_git_budget_is_spent_once_and_then_nothing_more_is_decoded() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        mkdirs(&root, &["Users/u/projects/repo/.git"]);
        let mut table: Vec<(String, String)> = Vec::new();
        for i in 0..6 {
            mkdirs(&root, &[&format!("Users/u/projects/repo/d{i}")]);
            table.push((
                format!("s{i}"),
                format!("/cc/projects/-Users-u-projects-repo-d{i}/s{i}.jsonl"),
            ));
        }
        let map: HashMap<String, Vec<String>> = table
            .into_iter()
            .map(|(id, path)| (id, vec![path]))
            .collect();
        let calls = std::cell::Cell::new(0usize);
        let counting = |dir: &Path, deadline: Instant| {
            calls.set(calls.get() + 1);
            nearest_git_dir(dir, deadline)
        };
        let widened = conversations_in_asker_repo_with(
            &map,
            &[PathBuf::from("/cc/projects")],
            &root.join("Users/u/projects/repo"),
            &root,
            &counting,
            roomy(),
        );
        assert_eq!(widened.len(), 3);
        // One probe for the asker, then one per folder inside the budget.
        assert_eq!(calls.get(), 1 + 3);
    }

    /// The MCP server's one-worker flag is cleared by this callback, so it has
    /// to fire when the worker ends and not when the caller stops waiting.
    #[test]
    fn the_worker_reports_when_it_is_over() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let over = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&over);
        let widened = conversations_in_client_repo(
            &HashMap::new(),
            Path::new("/cc/projects"),
            Path::new("/"),
            move || {
                seen.fetch_add(1, Ordering::SeqCst);
            },
        );
        assert!(widened.is_empty());
        let waited = Instant::now();
        while over.load(Ordering::SeqCst) == 0 && waited.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(over.load(Ordering::SeqCst), 1);
    }
}
