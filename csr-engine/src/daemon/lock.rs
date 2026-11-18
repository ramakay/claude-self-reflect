//! The daemon lock, `~/.claude-self-reflect/daemon.lock`: one daemon per user.

use std::path::PathBuf;

use anyhow::Result;

/// Is `path` a lock file some process holds exclusively? Opens it without
/// creating it and probes with a shared lock, released at once. A missing
/// file, or any error but contention, is "not held". Called through the fs2
/// trait: std's inherent `File::try_lock_shared` would win a method call and
/// report a different error type.
pub(crate) fn lock_file_held(path: &std::path::Path) -> bool {
    use fs2::FileExt;

    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    match FileExt::try_lock_shared(&file) {
        Ok(()) => {
            let _ = FileExt::unlock(&file);
            false
        }
        Err(e) => e.kind() == fs2::lock_contended_error().kind(),
    }
}

/// Take `file`'s exclusive lock, trying `tries` times `gap` apart while another
/// holder keeps it. `lock_file_held` probes the daemon lock with a shared lock
/// for an instant, so a daemon starting in that instant finds it contended; one
/// try would make it exit with "another daemon instance is already running".
/// Only the contended error is retried: any other error returns at once. Called
/// through the fs2 trait, as `lock_file_held` does (std's inherent
/// `File::try_lock_exclusive` would win a method call and report another type).
pub(super) fn lock_exclusive_retrying(
    file: &std::fs::File,
    tries: u32,
    gap: std::time::Duration,
) -> std::io::Result<()> {
    let contended = fs2::lock_contended_error().kind();
    let mut attempt = 0;
    loop {
        attempt += 1;
        match fs2::FileExt::try_lock_exclusive(file) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == contended && attempt < tries => std::thread::sleep(gap),
            Err(e) => return Err(e),
        }
    }
}

/// Acquire a lockfile to prevent multiple daemon instances.
/// Uses OS-level advisory locking (flock) via the fs2 crate. The winner writes
/// its pid into the file and records its start in the daemon history
/// (`crate::daemon_history`); a loser records nothing.
pub(super) fn acquire() -> Result<std::fs::File> {
    let lock_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".claude-self-reflect");
    crate::private_files::create_data_dir(&lock_dir)?;
    let lock_path = lock_dir.join("daemon.lock");

    // Not truncated on open: a second `csr-engine daemon` that fails to
    // lock must leave the running daemon's pid readable — MCP servers
    // check it (`vector_ipc::daemon_lock_live`) before proxying.
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)?;

    // Try a non-blocking exclusive lock — fails if another daemon holds it.
    // Retried for about a second: a hook's `daemon_lock_live` probe holds a
    // shared lock for an instant, which is not another daemon.
    lock_exclusive_retrying(&file, 20, std::time::Duration::from_millis(50)).map_err(|_| {
        anyhow::anyhow!(
            "another daemon instance is already running (lockfile: {})",
            lock_path.display()
        )
    })?;

    // Write PID (after acquiring lock)
    use std::io::Write;
    file.set_len(0)?;
    let mut locked = &file;
    write!(locked, "{}", std::process::id())?;

    crate::daemon_history::record_start_now();
    Ok(file)
}

#[cfg(test)]
mod lock_probe_tests {
    use super::{lock_exclusive_retrying, lock_file_held};
    use fs2::FileExt;
    use std::time::Duration;

    #[test]
    fn a_held_daemon_lock_reads_live_and_a_free_or_missing_one_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.lock");
        assert!(!lock_file_held(&path), "missing file");
        let file = std::fs::File::create(&path).unwrap();
        assert!(!lock_file_held(&path), "present but unlocked");
        FileExt::try_lock_exclusive(&file).unwrap();
        assert!(lock_file_held(&path), "held exclusively");
        FileExt::unlock(&file).unwrap();
        assert!(!lock_file_held(&path), "released");
    }

    #[test]
    fn a_starting_daemon_outwaits_a_probe_holding_a_shared_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.lock");
        let probe = std::fs::File::create(&path).unwrap();
        FileExt::try_lock_shared(&probe).unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            FileExt::unlock(&probe).unwrap();
        });
        let daemon = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        lock_exclusive_retrying(&daemon, 20, Duration::from_millis(50))
            .expect("the shared hold ended inside the window");
        release.join().unwrap();
        // and it holds the lock afterwards
        assert!(lock_file_held(&path));
    }

    #[test]
    fn a_real_exclusive_holder_still_fails_with_the_contended_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.lock");
        let holder = std::fs::File::create(&path).unwrap();
        FileExt::try_lock_exclusive(&holder).unwrap();
        let second = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        let err = lock_exclusive_retrying(&second, 4, Duration::from_millis(20))
            .expect_err("held for the whole window");
        assert_eq!(err.kind(), fs2::lock_contended_error().kind());
    }
}
