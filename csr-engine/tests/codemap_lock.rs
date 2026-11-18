//! Two processes, one repo: the per-repo scan lock (`codemap/lock.rs`).
//!
//! The test process holds a repo's lock file; the real `csr-engine codemap scan`
//! binary, run against a temp HOME and a temp database, must skip that repo after
//! its wait bound and write nothing, then map it once the lock is released.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use csr_engine::codemap::{identity, lock};

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

struct Sandbox {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    db: PathBuf,
    repo: PathBuf,
}

fn sandbox() -> Sandbox {
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let home = base.join("home");
    let repo = base.join("work").join("scratch");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &home, &["init", "-q", "-b", "main"]);
    std::fs::write(
        repo.join("lib.py"),
        "def locked_scan_probe(x):\n    return x\n",
    )
    .unwrap();
    git(&repo, &home, &["add", "-A"]);
    git(&repo, &home, &["commit", "-q", "-m", "one"]);
    Sandbox {
        db: base.join("csr-engine.db"),
        _tmp: tmp,
        home,
        repo,
    }
}

fn csr(sb: &Sandbox, args: &[&str], wait_secs: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_csr-engine"))
        .arg("--db-path")
        .arg(&sb.db)
        .args(args)
        .env("HOME", &sb.home)
        .env("CSR_CODEMAP_LOCK_WAIT_SECS", wait_secs)
        .env_remove("CSR_NO_CODEMAP")
        .env_remove("CSR_DISABLE_RECURSIVE_HOOKS")
        .output()
        .expect("csr-engine runs")
}

fn views(sb: &Sandbox) -> i64 {
    let conn = rusqlite::Connection::open(&sb.db).unwrap();
    conn.query_row("SELECT COUNT(*) FROM codemap_views", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn a_scan_skips_a_repo_another_process_holds_and_maps_it_once_released() {
    let sb = sandbox();
    let common = identity::identify_with(&sb.repo, None)
        .unwrap()
        .common_dir
        .to_string_lossy()
        .to_string();

    // First run creates the database (and maps the repo); drop its view so the
    // locked run below has something observable to NOT write.
    let warm = csr(
        &sb,
        &["codemap", "scan", "--repo", sb.repo.to_str().unwrap()],
        "5",
    );
    assert!(warm.status.success(), "{warm:?}");
    {
        let conn = rusqlite::Connection::open(&sb.db).unwrap();
        conn.execute_batch("DELETE FROM codemap_view_files; DELETE FROM codemap_views;")
            .unwrap();
    }
    assert_eq!(views(&sb), 0);

    let held = match lock::acquire(&sb.db, &common, Duration::ZERO, &|| false).unwrap() {
        lock::Acquired::Held(l) => l,
        other => panic!("lock not free: {other:?}"),
    };
    let t0 = std::time::Instant::now();
    let busy = csr(
        &sb,
        &["codemap", "scan", "--repo", sb.repo.to_str().unwrap()],
        "1",
    );
    assert!(busy.status.success(), "exits 0: {busy:?}");
    assert!(
        t0.elapsed() >= Duration::from_millis(900),
        "it waited the bound"
    );
    let stdout = String::from_utf8_lossy(&busy.stdout);
    assert!(
        stdout.contains(&format!("scan skipped: {common} locked by another scanner")),
        "{stdout}"
    );
    assert_eq!(views(&sb), 0, "a skipped scan writes no view");

    drop(held);
    let free = csr(
        &sb,
        &["codemap", "scan", "--repo", sb.repo.to_str().unwrap()],
        "1",
    );
    assert!(free.status.success(), "{free:?}");
    let stdout = String::from_utf8_lossy(&free.stdout);
    assert!(!stdout.contains("scan skipped"), "{stdout}");
    assert_eq!(views(&sb), 1, "released: the repo is mapped");
}
