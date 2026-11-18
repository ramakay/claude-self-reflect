//! A second `csr-engine daemon` must lose the lock before it does any heavy
//! work. launchd restarts a daemon that exits non-zero every 30 seconds, so a
//! loser that first opens the database and warms the embedding model would burn
//! CPU and memory on every retry. These run the real binary against a temp HOME.
#![cfg(unix)]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn a_losing_daemon_exits_before_it_opens_the_database_or_loads_the_model() {
    use fs2::FileExt;

    let home = tempfile::tempdir().unwrap();
    let data = home.path().join(".claude-self-reflect");
    std::fs::create_dir_all(&data).unwrap();
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(data.join("daemon.lock"))
        .unwrap();
    FileExt::try_lock_exclusive(&lock).unwrap();
    let db = home.path().join("elsewhere/csr-engine.db");

    let mut child = Command::new(env!("CARGO_BIN_EXE_csr-engine"))
        .arg("--db-path")
        .arg(&db)
        .arg("--projects-dir")
        .arg(home.path().join("projects"))
        .arg("daemon")
        .env("HOME", home.path())
        .env_remove("CSR_NO_DAEMON")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    // The lock retry takes about a second; an engine load would take far longer.
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let status = status.expect("the losing daemon was still starting up after 20s");
    let stderr = {
        use std::io::Read;
        let mut text = String::new();
        child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        text
    };
    assert!(!status.success(), "{stderr}");
    assert!(stderr.contains("another daemon instance"), "{stderr}");
    assert!(
        !db.exists(),
        "the loser must not have created the database: {stderr}"
    );
    drop(lock);
}
