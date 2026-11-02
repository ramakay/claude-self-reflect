//! Git facts for the SessionStart recap: is the working tree dirty, and is a
//! receipt commit on the checked-out HEAD.
//!
//! Everything here fails open. No repository, no `git`, a timeout, a failed
//! parse: the answer is "nothing to say", never an error and never a guess.
//! The cost is bounded: one `git status` call, then at most one
//! `git merge-base --is-ancestor` call per receipt, each with its own short
//! timeout and all sharing one overall deadline.

use std::collections::HashSet;
use std::io::Read;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

/// The status call. A tree too big to list in this time says nothing.
const STATUS_TIMEOUT: Duration = Duration::from_millis(400);
/// One ancestry check.
const ANCESTOR_TIMEOUT: Duration = Duration::from_millis(150);
/// All ancestry checks together.
const ANCESTOR_BUDGET: Duration = Duration::from_millis(500);
/// Most receipts checked.
const MAX_RECEIPTS: usize = 8;
/// Most paths named in the `Uncommitted:` line.
pub const MAX_NAMED_PATHS: usize = 3;
/// Longest path shown whole; longer ones keep their tail.
const MAX_PATH_CHARS: usize = 60;

/// What the working tree looks like to the recap.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoState {
    /// The checked-out branch; `None` when HEAD is detached.
    pub branch: Option<String>,
    /// Paths with uncommitted changes (modified, staged, untracked), in git's order.
    pub dirty: Vec<String>,
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

/// Read the repository at `cwd`: its status, then which of `receipts` are not
/// on HEAD. `None` when `cwd` is not in a repository (or git failed): no
/// marking, no dirty line.
pub fn probe(cwd: &Path, receipts: &[String]) -> Option<RepoState> {
    let out = run_git(
        cwd,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain",
            "--branch",
            "-z",
        ],
        STATUS_TIMEOUT,
    )?;
    if out.code != Some(0) {
        return None;
    }
    let (branch, dirty) = parse_status(&out.stdout)?;
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
    Some(RepoState {
        branch,
        dirty,
        off_head,
    })
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

/// `git status --porcelain --branch -z` output: the branch (`None` when
/// detached) and the changed paths. `None` when the output has no branch line.
pub(crate) fn parse_status(stdout: &[u8]) -> Option<(Option<String>, Vec<String>)> {
    let text = String::from_utf8_lossy(stdout);
    let mut records = text.split('\0');
    let head = records.next()?.strip_prefix("## ")?;
    let branch = if head.starts_with("HEAD (no branch)") {
        None
    } else if let Some(name) = head
        .strip_prefix("No commits yet on ")
        .or_else(|| head.strip_prefix("Initial commit on "))
    {
        Some(name.trim().to_string())
    } else {
        let name = head.split("...").next().unwrap_or(head);
        Some(name.split(' ').next().unwrap_or(name).to_string())
    };
    let mut dirty = Vec::new();
    while let Some(record) = records.next() {
        if record.len() < 4 || !record.is_char_boundary(3) {
            continue;
        }
        let (code, path) = record.split_at(3);
        // A rename or copy carries its source path as the next record.
        if code.starts_with(['R', 'C']) || code[1..2].starts_with(['R', 'C']) {
            let _ = records.next();
        }
        // Ignored files (`!!`) are not changes.
        if code.starts_with("!!") {
            continue;
        }
        dirty.push(path.to_string());
    }
    Some((branch, dirty))
}

/// `Uncommitted: N files (a, b, c, +k more)`, or `None` for a clean tree.
pub fn uncommitted_line(dirty: &[String]) -> Option<String> {
    if dirty.is_empty() {
        return None;
    }
    let mut named: Vec<String> = dirty
        .iter()
        .take(MAX_NAMED_PATHS)
        .map(|path| shorten_path(path))
        .collect();
    if dirty.len() > MAX_NAMED_PATHS {
        named.push(format!("+{} more", dirty.len() - MAX_NAMED_PATHS));
    }
    Some(format!(
        "Uncommitted: {} {} ({})",
        dirty.len(),
        if dirty.len() == 1 { "file" } else { "files" },
        named.join(", ")
    ))
}

/// A path on one line, without control characters, at most
/// [`MAX_PATH_CHARS`] long (the tail is kept: it names the file).
fn shorten_path(path: &str) -> String {
    let clean: String = path
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let clean = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    let count = clean.chars().count();
    if count <= MAX_PATH_CHARS {
        return clean;
    }
    let tail: String = clean.chars().skip(count - (MAX_PATH_CHARS - 1)).collect();
    format!("…{tail}")
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
    fn status_parses_branch_paths_renames_and_detached_head() {
        let (branch, dirty) = parse_status(
            b"## main...origin/main [ahead 1]\0 M src/a.rs\0?? new.txt\0R  new_name\0old_name\0",
        )
        .unwrap();
        assert_eq!(branch.as_deref(), Some("main"));
        assert_eq!(dirty, vec!["src/a.rs", "new.txt", "new_name"]);
        let (branch, dirty) = parse_status(b"## HEAD (no branch)\0").unwrap();
        assert_eq!(branch, None);
        assert!(dirty.is_empty());
        let (branch, _) = parse_status(b"## No commits yet on trunk\0").unwrap();
        assert_eq!(branch.as_deref(), Some("trunk"));
        assert!(parse_status(b"not status output").is_none());
    }

    #[test]
    fn uncommitted_line_caps_the_named_paths() {
        assert_eq!(uncommitted_line(&[]), None);
        let one = vec!["a.rs".to_string()];
        assert_eq!(
            uncommitted_line(&one).unwrap(),
            "Uncommitted: 1 file (a.rs)"
        );
        let five: Vec<String> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            uncommitted_line(&five).unwrap(),
            "Uncommitted: 5 files (a, b, c, +2 more)"
        );
        let long = vec![format!("{}/file.rs", "d".repeat(100))];
        let line = uncommitted_line(&long).unwrap();
        assert!(line.contains("…") && line.ends_with("file.rs)"), "{line}");
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
        assert!(state.dirty.is_empty());
    }

    #[test]
    fn probe_reports_a_dirty_tree_and_a_detached_head() {
        let dir = repo();
        std::fs::write(dir.path().join("a.txt"), "changed").unwrap();
        std::fs::write(dir.path().join("new.txt"), "n").unwrap();
        let state = probe(dir.path(), &[]).unwrap();
        assert_eq!(state.dirty, vec!["a.txt", "new.txt"]);
        let head = git(dir.path(), &["rev-parse", "HEAD"]);
        git(dir.path(), &["checkout", "-q", "--detach", &head]);
        let state = probe(dir.path(), &[]).unwrap();
        assert_eq!(state.branch, None);
        assert_eq!(state.head_label(), "HEAD");
    }

    #[test]
    fn probe_without_a_repository_says_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(probe(dir.path(), &["abcdef1".to_string()]).is_none());
    }
}
