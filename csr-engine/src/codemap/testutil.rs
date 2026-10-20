//! Test helpers: real git repos under a temp dir, driven by the git CLI.

use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::Connection;

pub(crate) struct Fx {
    /// Holds the temp dir alive; repos live under `root`.
    _tmp: tempfile::TempDir,
    pub root: PathBuf,
    /// A throwaway HOME so git never reads the real one.
    pub home: PathBuf,
}

impl Fx {
    pub fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(tmp.path()).unwrap();
        let home = base.join("home");
        let root = base.join("work");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        Self {
            _tmp: tmp,
            root,
            home,
        }
    }

    pub fn git(&self, dir: &Path, args: &[&str]) -> String {
        // An absolute program keeps std on posix_spawn. A bare name plus a
        // changed PATH makes it fork+exec, and every forked child holds the
        // whole test process's flocks (maintenance locks of other tests)
        // until its exec.
        let out = Command::new(git_program())
            .args(args)
            .current_dir(dir)
            .env_clear()
            .env("PATH", GIT_PATH)
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?} in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// `git init` a repo at `<root>/<name>` and return its real path.
    pub fn init_repo(&self, name: &str) -> PathBuf {
        let dir = self.root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        self.git(&dir, &["init", "-q", "-b", "main"]);
        std::fs::canonicalize(&dir).unwrap()
    }

    pub fn commit_all(&self, dir: &Path, msg: &str) {
        self.git(dir, &["add", "-A"]);
        self.git(dir, &["commit", "-q", "-m", msg]);
    }

    /// Add a linked worktree at `path` on a new branch.
    pub fn add_worktree(&self, main: &Path, path: &Path, branch: &str) -> PathBuf {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        self.git(
            main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                branch,
                &path.to_string_lossy(),
            ],
        );
        std::fs::canonicalize(path).unwrap()
    }
}

pub(crate) fn write(dir: &Path, rel: &str, body: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

pub(crate) fn db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    crate::storage::migrations::run(&conn).unwrap();
    conn
}

pub(crate) fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

/// A repo with a main checkout and two linked worktrees, not yet scanned:
/// `main` and `same` hold identical files; `diff` has an uncommitted edit of
/// `src/radio.py` (so its blob differs and it is dirty).
pub(crate) struct MapFixture {
    pub fx: Fx,
    pub main: PathBuf,
    pub same: PathBuf,
    pub diff: PathBuf,
}

pub(crate) fn map_fixture() -> MapFixture {
    let fx = Fx::new();
    let main = fx.init_repo("mapfx");
    write(&main, "src/radio.py", &py(&["alpha_one", "beta_two"]));
    write(&main, "src/only.py", &py(&["gamma_three"]));
    write(&main, "lib/util/helpers.py", &py(&["help_me"]));
    write(&main, "pkg_a/dup.py", &py(&["dup_a"]));
    write(&main, "pkg_b/dup.py", &py(&["dup_b"]));
    for i in 0..12 {
        write(
            &main,
            &format!("many/m{i:02}/same_name.py"),
            &py(&["many_fn"]),
        );
    }
    fx.commit_all(&main, "init");
    let same = fx.add_worktree(&main, &fx.root.join("mapfx-same"), "same");
    let diff = fx.add_worktree(&main, &fx.root.join("mapfx-diff"), "diff");
    write(
        &diff,
        "src/radio.py",
        &py(&["alpha_one", "beta_two", "delta_four"]),
    );
    MapFixture {
        fx,
        main,
        same,
        diff,
    }
}

/// Scan every view of the repo whose main checkout is `repo` into `conn`.
pub(crate) fn scan_repo_into(conn: &Connection, fx: &Fx, repo: &Path) {
    super::scan::scan_repo(
        conn,
        &repo.join(".git"),
        &super::scan::ScanOpts {
            home: Some(fx.home.clone()),
            ..Default::default()
        },
    )
    .unwrap();
}

/// Makes a directory unreadable (mode 000) until dropped, so a test proves a
/// protected folder was not opened: any read of it fails loudly. The mode is
/// restored on drop so tempdir cleanup works.
pub(crate) struct Locked {
    path: PathBuf,
}

impl Locked {
    pub fn new(path: &Path) -> Self {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
        Self {
            path: path.to_path_buf(),
        }
    }
}

impl Drop for Locked {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o755));
    }
}

/// A Python source defining the given functions.
pub(crate) fn py(defs: &[&str]) -> String {
    defs.iter()
        .map(|d| format!("def {d}(x):\n    return x\n\n"))
        .collect()
}

const GIT_PATH: &str = "/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin";

/// `git` resolved against [`GIT_PATH`] once.
fn git_program() -> &'static Path {
    static GIT: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    GIT.get_or_init(|| {
        GIT_PATH
            .split(':')
            .map(|dir| Path::new(dir).join("git"))
            .find(|candidate| candidate.is_file())
            .unwrap_or_else(|| "git".into())
    })
}
