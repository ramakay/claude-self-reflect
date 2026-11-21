//! Git facts for the SessionStart recap: is a receipt commit on the checked-out
//! HEAD.
//!
//! Everything here fails open. No repository, no `git`, a timeout, a failed
//! parse: the answer is "nothing to say", never an error and never a guess.
//! The cost is bounded: no call at all when the recap has no commit receipt to
//! check; otherwise one `git symbolic-ref` call (a read of `.git/HEAD`, never a
//! scan of the working tree), then at most one `git merge-base --is-ancestor`
//! call per receipt, each with its own short timeout and all sharing one
//! overall deadline.

use std::collections::HashSet;
use std::io::Read;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

/// The branch read.
const BRANCH_TIMEOUT: Duration = Duration::from_millis(150);
/// One ancestry check.
const ANCESTOR_TIMEOUT: Duration = Duration::from_millis(150);
/// All ancestry checks together.
const ANCESTOR_BUDGET: Duration = Duration::from_millis(300);
/// Most receipts checked.
const MAX_RECEIPTS: usize = 8;

/// What the checked-out HEAD looks like to the recap.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoState {
    /// The checked-out branch; `None` when HEAD is detached.
    pub branch: Option<String>,
    /// Receipts proved not to be ancestors of HEAD.
    pub off_head: HashSet<String>,
}

impl RepoState {
    /// What HEAD is called in `not on <label>`.
    pub fn head_label(&self) -> String {
        self.branch.clone().unwrap_or_else(|| "HEAD".to_string())
    }

    /// The recap context the composer reads.
    pub fn recap_context(&self, session_is_live: bool) -> super::recap::RecapContext {
        super::recap::RecapContext {
            off_head: self.off_head.clone(),
            head_label: self.head_label(),
            session_is_live,
        }
    }
}

/// Read the repository at `cwd`: its branch, then which of `receipts` are not
/// on HEAD. `None` when there is no commit receipt to check, when `cwd` is not
/// in a repository, or when git failed: no marking.
pub fn probe(cwd: &Path, receipts: &[String]) -> Option<RepoState> {
    if !receipts.iter().any(|receipt| is_hex_oid(receipt)) {
        return None;
    }
    // Exit 0: on a branch. Exit 1: detached HEAD. Anything else (128: not a
    // repository) is "cannot tell".
    let head = run_git(
        cwd,
        &["symbolic-ref", "--short", "-q", "HEAD"],
        BRANCH_TIMEOUT,
    )?;
    let branch = match head.code {
        Some(0) => {
            let name = String::from_utf8_lossy(&head.stdout).trim().to_string();
            (!name.is_empty()).then_some(name)
        }
        Some(1) => None,
        _ => return None,
    };
    let mut off_head = HashSet::new();
    let deadline = Instant::now() + ANCESTOR_BUDGET;
    for receipt in receipts.iter().take(MAX_RECEIPTS) {
        if Instant::now() >= deadline {
            break;
        }
        if !is_hex_oid(receipt) {
            continue;
        }
        let Some(check) = run_git(
            cwd,
            &["merge-base", "--is-ancestor", receipt, "HEAD"],
            ANCESTOR_TIMEOUT,
        ) else {
            continue;
        };
        // 0: an ancestor. 1: not one. Anything else (128: unknown object, no
        // HEAD) is "cannot tell", which is shown unmarked.
        if check.code == Some(1) {
            off_head.insert(receipt.trim().to_string());
        }
    }
    Some(RepoState { branch, off_head })
}

fn is_hex_oid(text: &str) -> bool {
    let text = text.trim();
    (7..=40).contains(&text.len()) && text.chars().all(|c| c.is_ascii_hexdigit())
}

struct GitOutput {
    code: Option<i32>,
    stdout: Vec<u8>,
}

/// Run git in `cwd` with a deadline. `None` when it cannot start or runs out
/// of time (the child is killed).
fn run_git(cwd: &Path, args: &[&str], timeout: Duration) -> Option<GitOutput> {
    let mut command = crate::import::backfill::git_at(&cwd.to_string_lossy());
    let mut child = command
        .args(args)
        .env("LC_ALL", "C")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut pipe = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        bytes
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            // The reader ends once the killed child's pipe closes.
            let _ = reader.join();
            return None;
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    let stdout = reader.join().ok()?;
    Some(GitOutput {
        code: status.code(),
        stdout,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(repo: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .expect("git runs");
        assert!(out.status.success(), "git {args:?}: {:?}", out);
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-q", "-m", "one"]);
        dir
    }

    #[test]
    fn probe_marks_only_receipts_that_are_not_ancestors_of_head() {
        let dir = repo();
        let on_main = git(dir.path(), &["rev-parse", "HEAD"]);
        git(dir.path(), &["checkout", "-q", "-b", "side"]);
        std::fs::write(dir.path().join("b.txt"), "b").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-q", "-m", "side work"]);
        let on_side = git(dir.path(), &["rev-parse", "HEAD"]);
        git(dir.path(), &["checkout", "-q", "main"]);
        let unknown = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef".to_string();
        let state = probe(
            dir.path(),
            &[
                on_main.clone(),
                on_side[..9].to_string(),
                unknown.clone(),
                "not-hex!".into(),
            ],
        )
        .expect("a repository");
        assert_eq!(state.branch.as_deref(), Some("main"));
        assert!(state.off_head.contains(&on_side[..9]));
        assert!(
            !state.off_head.contains(&on_main),
            "an ancestor is unmarked"
        );
        assert!(
            !state.off_head.contains(&unknown),
            "an unknown object is unmarked"
        );
    }

    #[test]
    fn probe_names_a_detached_head() {
        let dir = repo();
        let head = git(dir.path(), &["rev-parse", "HEAD"]);
        git(dir.path(), &["checkout", "-q", "--detach", &head]);
        let state = probe(dir.path(), std::slice::from_ref(&head)).unwrap();
        assert_eq!(state.branch, None);
        assert_eq!(state.head_label(), "HEAD");
    }

    #[test]
    fn probe_reads_no_working_tree_state() {
        // A dirty tree adds nothing: the probe carries no path list at all, and
        // with no commit receipt to check it does not start git.
        let dir = repo();
        std::fs::write(dir.path().join("a.txt"), "changed").unwrap();
        std::fs::write(dir.path().join("new.txt"), "n").unwrap();
        assert!(probe(dir.path(), &[]).is_none());
        assert!(probe(dir.path(), &["not-hex!".to_string()]).is_none());
        let head = git(dir.path(), &["rev-parse", "HEAD"]);
        let state = probe(dir.path(), &[head]).unwrap();
        assert_eq!(state, RepoState::default().with_branch("main"));
    }

    #[test]
    fn probe_without_a_repository_says_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(probe(dir.path(), &["abcdef1".to_string()]).is_none());
    }

    impl RepoState {
        fn with_branch(mut self, name: &str) -> Self {
            self.branch = Some(name.to_string());
            self
        }
    }
}
