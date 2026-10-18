//! The code map for a session inside a macOS-protected folder, end to end: the real
//! binary against a FAKE home whose Desktop/Documents/Downloads are plain temp
//! directories. Nothing here touches the real protected folders or the real
//! database, and every child process gets the fake HOME.

#![cfg(unix)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

struct Fake {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    db: PathBuf,
    projects: PathBuf,
    probe: PathBuf,
}

fn git(dir: &Path, home: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin")
        .env("HOME", home)
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
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn fake() -> Fake {
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let home = base.join("home");
    for d in ["Desktop", "Documents", "Downloads"] {
        std::fs::create_dir_all(home.join(d)).unwrap();
    }
    let probe = home.join("Documents/probe");
    std::fs::create_dir_all(&probe).unwrap();
    git(&probe, &home, &["init", "-q", "-b", "main"]);
    std::fs::write(probe.join("app.py"), "def alpha_marker():\n    return 1\n").unwrap();
    git(&probe, &home, &["add", "-A"]);
    git(&probe, &home, &["commit", "-q", "-m", "one"]);
    let data = base.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(base.join("projects")).unwrap();
    Fake {
        db: data.join("csr-engine.db"),
        projects: base.join("projects"),
        home,
        probe,
        _tmp: tmp,
    }
}

impl Fake {
    fn cmd(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_csr-engine"));
        c.arg("--db-path")
            .arg(&self.db)
            .env("HOME", &self.home)
            .env_remove("CSR_NO_CODEMAP")
            .env_remove("CSR_DISABLE_RECURSIVE_HOOKS")
            .env_remove("CSR_CODEMAP_REPOS")
            .stdin(Stdio::null());
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd().args(args).output().unwrap()
    }

    fn hook(&self, name: &str, payload: serde_json::Value) -> Output {
        let mut child = self
            .cmd()
            .args(["hook", name, "--projects-dir"])
            .arg(&self.projects)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    fn query(&self, name: &str) -> Vec<serde_json::Value> {
        let out = self.run(&["codemap", "query", name, "--json"]);
        serde_json::from_slice(&out.stdout).unwrap_or_default()
    }

    fn wait_for(&self, secs: u64, mut f: impl FnMut(&Fake) -> bool) -> bool {
        let end = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < end {
            if f(self) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        f(self)
    }

    fn session_locks(&self) -> Vec<PathBuf> {
        let dir = self.db.parent().unwrap().join("codemap");
        std::fs::read_dir(dir)
            .map(|d| {
                d.flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.file_name()
                            .is_some_and(|n| n.to_string_lossy().starts_with("session-"))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn a_plain_scan_refuses_a_protected_repo_and_a_session_scan_maps_it() {
    let f = fake();
    let refused = f.run(&["codemap", "scan", "--repo", f.probe.to_str().unwrap()]);
    assert!(!refused.status.success(), "{}", text(&refused));
    assert!(
        text(&refused).contains("macOS-protected"),
        "{}",
        text(&refused)
    );
    assert!(f.query("alpha_marker").is_empty());

    let scanned = f.run(&[
        "codemap",
        "scan",
        "--repo",
        f.probe.to_str().unwrap(),
        "--session",
    ]);
    assert!(scanned.status.success(), "{}", text(&scanned));
    assert_eq!(f.query("alpha_marker").len(), 1);
    let status = f.run(&["codemap", "status", "--json"]);
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["repos"][0]["source"], "session", "{status}");
    // The report lists the repo's views and, next to them, why the daemon skips it.
    assert!(
        !status["repos"][0]["views"].as_array().unwrap().is_empty(),
        "{status}"
    );
    assert_eq!(
        status["skipped"],
        serde_json::json!([{
            "path": f.probe.to_str().unwrap(),
            "reason": "protected-folder: mapped by session only",
        }]),
        "{status}"
    );
    let text = f.run(&["codemap", "status"]);
    let text = String::from_utf8_lossy(&text.stdout).into_owned();
    assert!(
        text.lines()
            .any(|l| l.starts_with("repo ")
                && l.ends_with("[protected-folder: mapped by session only]")),
        "{text}"
    );
}

#[test]
fn a_session_scan_does_not_bring_back_a_repo_the_user_removed() {
    let f = fake();
    let probe = f.probe.to_str().unwrap();
    assert!(f
        .run(&["codemap", "scan", "--repo", probe, "--session"])
        .status
        .success());
    // `codemap remove` on a protected path is refused too, so tombstone through
    // the stored row, the way discovery's `enabled = 0` rows are written.
    let conn = rusqlite::Connection::open(&f.db).unwrap();
    conn.execute("UPDATE codemap_repos SET enabled = 0", [])
        .unwrap();
    drop(conn);
    let again = f.run(&["codemap", "scan", "--repo", probe, "--session"]);
    assert!(again.status.success(), "{}", text(&again));
    assert!(text(&again).contains("codemap remove"), "{}", text(&again));
    let conn = rusqlite::Connection::open(&f.db).unwrap();
    let enabled: i64 = conn
        .query_row("SELECT enabled FROM codemap_repos", [], |r| r.get(0))
        .unwrap();
    assert_eq!(enabled, 0, "the session scan left the tombstone alone");
}

#[test]
fn session_start_in_a_protected_cwd_starts_a_detached_scan_and_returns_at_once() {
    let f = fake();
    let started = Instant::now();
    let out = f.hook(
        "session-start",
        serde_json::json!({
            "session_id": "sess-protected",
            "cwd": f.probe,
            "source": "startup",
        }),
    );
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        f.wait_for(30, |f| !f.query("alpha_marker").is_empty()),
        "the detached child mapped the repo within 30 s (hook took {:?})",
        started.elapsed()
    );
    // The signal is on record and says the daemon should leave it alone.
    let conn = rusqlite::Connection::open(&f.db).unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM codemap_signals WHERE source = 'session_start'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn an_edit_in_a_protected_repo_is_queued_drained_by_a_detached_child_and_the_lock_goes() {
    let f = fake();
    let probe = f.probe.to_str().unwrap();
    assert!(f
        .run(&["codemap", "scan", "--repo", probe, "--session"])
        .status
        .success());
    assert!(f.query("beta_marker").is_empty());

    // A dirty edit: not committed, HEAD and the index do not move.
    std::fs::write(
        f.probe.join("app.py"),
        "def alpha_marker():\n    return 1\n\ndef beta_marker():\n    return 2\n",
    )
    .unwrap();
    let file = f.probe.join("app.py");
    let out = f.hook(
        "post-tool-use",
        serde_json::json!({
            "session_id": "sess-protected",
            "cwd": f.probe,
            "tool_name": "Edit",
            "tool_input": { "file_path": file },
        }),
    );
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        !f.session_locks().is_empty() || !f.query("beta_marker").is_empty(),
        "the hook took a lock for the drain it started (or the drain already finished)"
    );
    assert!(
        f.wait_for(40, |f| !f.query("beta_marker").is_empty()),
        "the drain rescanned the edited file"
    );
    assert!(
        f.wait_for(40, |f| f.session_locks().is_empty()),
        "the child removed its lock file on exit: {:?}",
        f.session_locks()
    );
}

#[test]
fn the_daemon_loop_never_polls_watches_or_drains_a_protected_repo() {
    let f = fake();
    let probe = f.probe.to_str().unwrap();
    assert!(f
        .run(&["codemap", "scan", "--repo", probe, "--session"])
        .status
        .success());
    // A protected queue row, as a PostToolUse hook leaves it.
    let conn = rusqlite::Connection::open(&f.db).unwrap();
    conn.execute(
        "INSERT INTO codemap_queue (path, enqueued_at) VALUES (?1, ?2)",
        rusqlite::params![f.probe.join("app.py").to_str().unwrap(), i64::MAX / 2],
    )
    .unwrap();
    drop(conn);

    let out = f.run(&["codemap", "watch", "--for", "8"]);
    let log = text(&out);
    assert!(out.status.success(), "{log}");
    assert!(log.contains("poll repos=0"), "no active repo: {log}");
    assert!(
        !log.contains("probe"),
        "no line names the protected repo: {log}"
    );
    assert!(!log.contains("watch +"), "nothing is watched: {log}");
    let conn = rusqlite::Connection::open(&f.db).unwrap();
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM codemap_queue", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 1, "the protected queue row is left for its session");
}

#[test]
fn a_session_in_an_ordinary_folder_spawns_nothing() {
    let f = fake();
    let plain = f.home.join("projects/plain");
    std::fs::create_dir_all(&plain).unwrap();
    git(&plain, &f.home, &["init", "-q", "-b", "main"]);
    let out = f.hook(
        "session-start",
        serde_json::json!({ "session_id": "s", "cwd": plain, "source": "startup" }),
    );
    assert!(out.status.success(), "{}", text(&out));
    std::thread::sleep(Duration::from_secs(2));
    assert!(f.query("alpha_marker").is_empty());
    assert!(f.session_locks().is_empty());
}
