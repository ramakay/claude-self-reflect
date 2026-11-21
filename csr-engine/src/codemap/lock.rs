//! Cross-process scan lock: one exclusive file lock per repo.
//!
//! The daemon's poller and pump are two threads of one process, but they are not
//! the only scanners. A user's `csr-engine codemap scan --repo X` can race the
//! daemon on X, and the session children (`codemap scan --session`, `codemap
//! drain`) run in processes of their own. A full scan reads a file's blob A, a
//! partial rescan can then commit blob B, and the scan would commit A over it; a
//! dirty edit moves neither HEAD nor the index, so nothing would ever correct it.
//!
//! The lock file is `<data_dir>/codemap/locks/<16 hex of blake3(common dir)>.lock`,
//! taken with `flock` (`fs2`). `flock` conflicts across open file descriptions, so
//! it also serialises two threads of one process, and the kernel drops it when a
//! holder dies. That is why it replaced the in-process `scan_lock` mutex: one
//! mechanism covers every pair of scanners.
//!
//! Lock order everywhere: the daemon's heavy-work permit, then the repo file
//! lock. A scanner never waits for the permit while it holds a repo lock.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use fs2::FileExt;

/// How long a scanner waits for another scanner of the same repo
/// (`CSR_CODEMAP_LOCK_WAIT_SECS`, default 30).
pub const DEFAULT_WAIT: Duration = Duration::from_secs(30);

/// How often a waiting scanner retries the lock.
const POLL: Duration = Duration::from_millis(50);

/// The wait bound: `CSR_CODEMAP_LOCK_WAIT_SECS`, or `DEFAULT_WAIT`.
pub fn wait_from_env() -> Duration {
    wait_from_value(std::env::var("CSR_CODEMAP_LOCK_WAIT_SECS").ok().as_deref())
}

pub(crate) fn wait_from_value(value: Option<&str>) -> Duration {
    value
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_WAIT)
}

/// Directory that holds the per-repo lock files, next to the database.
pub fn lock_dir(db_path: &Path) -> PathBuf {
    let data_dir = match db_path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    data_dir.join("codemap").join("locks")
}

/// The lock file of one repo.
pub fn lock_path(db_path: &Path, common_dir: &str) -> PathBuf {
    let hash = blake3::hash(common_dir.as_bytes()).to_hex();
    lock_dir(db_path).join(format!("{}.lock", &hash.as_str()[..16]))
}

/// A held repo lock. Released when dropped.
#[derive(Debug)]
pub struct RepoLock {
    file: File,
}

impl Drop for RepoLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

/// What waiting for a repo lock came to.
#[derive(Debug)]
pub enum Acquired {
    Held(RepoLock),
    /// Another scanner held it for the whole wait.
    Busy,
    /// `cancel` turned true first (the daemon is shutting down).
    Cancelled,
}

/// The one-line message a skipped scan logs or prints.
pub fn skipped_line(repo: &str) -> String {
    format!("scan skipped: {repo} locked by another scanner")
}

/// Take the exclusive lock of `common_dir`, polling for at most `wait` and
/// checking `cancel` between tries.
pub fn acquire(
    db_path: &Path,
    common_dir: &str,
    wait: Duration,
    cancel: &dyn Fn() -> bool,
) -> Result<Acquired> {
    let path = lock_path(db_path, common_dir);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    let deadline = Instant::now() + wait;
    loop {
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(Acquired::Held(RepoLock { file })),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e).with_context(|| format!("lock {}", path.display())),
        }
        if cancel() {
            return Ok(Acquired::Cancelled);
        }
        if Instant::now() >= deadline {
            return Ok(Acquired::Busy);
        }
        std::thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn never() -> bool {
        false
    }

    #[test]
    fn the_wait_bound_reads_whole_seconds_and_defaults_to_thirty() {
        assert_eq!(wait_from_value(None), Duration::from_secs(30));
        assert_eq!(wait_from_value(Some("1")), Duration::from_secs(1));
        assert_eq!(wait_from_value(Some(" 0 ")), Duration::ZERO);
        assert_eq!(wait_from_value(Some("soon")), DEFAULT_WAIT);
        assert_eq!(wait_from_value(Some("")), DEFAULT_WAIT);
    }

    #[test]
    fn the_lock_file_name_is_sixteen_hex_of_the_common_dir_next_to_the_database() {
        let db = Path::new("/data/csr-engine.db");
        let a = lock_path(db, "/w/r/.git");
        let b = lock_path(db, "/w/other/.git");
        assert_ne!(a, b);
        assert_eq!(a, lock_path(db, "/w/r/.git"), "stable");
        assert_eq!(a.parent().unwrap(), Path::new("/data/codemap/locks"));
        let stem = a.file_stem().unwrap().to_string_lossy().to_string();
        assert_eq!(stem.len(), 16);
        assert!(stem.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(a.extension().unwrap(), "lock");
        // A bare file name still gets a directory.
        assert_eq!(
            lock_dir(Path::new("csr.db")),
            Path::new("./codemap/locks").to_path_buf()
        );
    }

    #[test]
    fn a_second_scanner_waits_the_bound_then_reports_busy_and_gets_it_after_release() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("csr-engine.db");
        let first = match acquire(&db, "/w/r/.git", Duration::ZERO, &never).unwrap() {
            Acquired::Held(l) => l,
            other => panic!("free lock not taken: {other:?}"),
        };
        let t0 = Instant::now();
        let second = acquire(&db, "/w/r/.git", Duration::from_millis(300), &never).unwrap();
        assert!(matches!(second, Acquired::Busy), "{second:?}");
        assert!(t0.elapsed() >= Duration::from_millis(250), "it waited");
        // Another repo is independent.
        assert!(matches!(
            acquire(&db, "/w/other/.git", Duration::ZERO, &never).unwrap(),
            Acquired::Held(_)
        ));
        drop(first);
        assert!(matches!(
            acquire(&db, "/w/r/.git", Duration::ZERO, &never).unwrap(),
            Acquired::Held(_)
        ));
    }

    #[test]
    fn a_cancel_ends_the_wait_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("csr-engine.db");
        let _held = acquire(&db, "/w/r/.git", Duration::ZERO, &never).unwrap();
        let t0 = Instant::now();
        let got = acquire(&db, "/w/r/.git", Duration::from_secs(30), &|| true).unwrap();
        assert!(matches!(got, Acquired::Cancelled), "{got:?}");
        assert!(t0.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn two_threads_of_one_process_exclude_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("csr-engine.db");
        let held = acquire(&db, "/w/r/.git", Duration::ZERO, &never).unwrap();
        let other = std::thread::scope(|s| {
            s.spawn(|| acquire(&db, "/w/r/.git", Duration::from_millis(100), &never).unwrap())
                .join()
                .unwrap()
        });
        assert!(matches!(other, Acquired::Busy), "{other:?}");
        drop(held);
    }
}
