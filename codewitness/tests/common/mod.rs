//! Shared fixture-building helpers for the adversarial integration tests.
//!
//! Deliberately dependency-free (no `tempfile` crate): a unique directory
//! under `std::env::temp_dir()` is created by hand and removed on `Drop`.
//! Git itself is driven via `std::process::Command` — these tests exercise
//! `codewitness` against *real* git history, not a mock.

use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

pub struct TempRepo {
    root: PathBuf,
    pub path: PathBuf,
}

impl TempRepo {
    pub fn new() -> Self {
        let root = unique_tmp_dir("codewitness-test");
        let path = root.join("repo");
        let home = root.join("home");
        std::fs::create_dir_all(&path).expect("create repo dir");
        std::fs::create_dir_all(&home).expect("create fake home dir");

        let repo = Self { root, path };
        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&["config", "user.name", "Codewitness Test"]);
        repo.git(&["config", "user.email", "test@codewitness.invalid"]);
        repo.git(&["config", "commit.gpgsign", "false"]);
        repo.git(&["config", "tag.gpgsign", "false"]);
        repo
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    pub fn git(&self, args: &[&str]) -> Output {
        self.git_with_env(args, &[])
    }

    pub fn git_with_env(&self, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new("git");
        cmd.current_dir(&self.path)
            .args(args)
            // Isolate from whatever git config/hooks/aliases happen to be
            // installed on the machine running the tests.
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", self.home())
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_AUTHOR_DATE")
            .env_remove("GIT_COMMITTER_DATE");
        for (key, value) in extra_env {
            cmd.env(key, value);
        }
        let output = cmd.output().expect("failed to spawn git");
        if !output.status.success() {
            panic!(
                "`git {}` failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
                args.join(" "),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
        }
        output
    }

    pub fn write(&self, rel_path: &str, content: &str) {
        let full = self.path.join(rel_path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("create parent dirs");
        }
        std::fs::write(full, content).expect("write fixture file");
    }

    pub fn commit_all(&self, message: &str) -> codewitness::ObjectId {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
        self.head()
    }

    /// Commit with an explicitly lying author/committer date — the whole
    /// point of the rebase-with-lying-dates fixture.
    pub fn commit_all_dated(
        &self,
        message: &str,
        author_date: &str,
        committer_date: &str,
    ) -> codewitness::ObjectId {
        self.git(&["add", "-A"]);
        self.git_with_env(
            &["commit", "-q", "-m", message],
            &[
                ("GIT_AUTHOR_DATE", author_date),
                ("GIT_COMMITTER_DATE", committer_date),
            ],
        );
        self.head()
    }

    pub fn head(&self) -> codewitness::ObjectId {
        self.rev_parse("HEAD")
    }

    pub fn rev_parse(&self, spec: &str) -> codewitness::ObjectId {
        let out = self.git(&["rev-parse", spec]);
        String::from_utf8(out.stdout)
            .expect("git rev-parse produced non-utf8 output")
            .trim()
            .parse()
            .expect("git rev-parse produced a non-hash-shaped line")
    }

    pub fn checkout(&self, name: &str) {
        self.git(&["checkout", "-q", name]);
    }

    pub fn checkout_new(&self, name: &str) {
        self.git(&["checkout", "-q", "-b", name]);
    }

    pub fn cherry_pick(&self, commit: codewitness::ObjectId) {
        self.git(&["cherry-pick", &commit.to_string()]);
    }

    /// `git rebase <target>` with a forced (lying) committer date applied to
    /// every replayed commit.
    pub fn rebase_onto(&self, target: &str, lying_committer_date: &str) {
        self.git_with_env(
            &["rebase", target],
            &[("GIT_COMMITTER_DATE", lying_committer_date)],
        );
    }

    /// Committer-date (unix seconds) of `spec`, straight from git — used to
    /// prove that the lying date really did land where the test expects,
    /// i.e. that naive timestamp ordering really would get this wrong.
    pub fn committer_time(&self, spec: &str) -> i64 {
        let out = self.git(&["show", "-s", "--format=%ct", spec]);
        String::from_utf8(out.stdout)
            .expect("git show produced non-utf8 output")
            .trim()
            .parse()
            .expect("git show produced a non-numeric committer time")
    }

    pub fn auditor(&self) -> codewitness::Auditor {
        codewitness::Auditor::open(&self.path).expect("open repo for auditing")
    }
}

impl Drop for TempRepo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn unique_tmp_dir(prefix: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before UNIX_EPOCH")
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}-{}-{n}-{nanos}", std::process::id()))
}
