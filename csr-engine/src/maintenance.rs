//! One-shot maintenance runs (`backfill scrub`) quiesce the database instead of
//! racing the processes that write it.
//!
//! Two parts. A preflight ([`other_holders`]) lists every other process that has
//! the database (or its WAL) open; the scrub refuses to start while any does,
//! because a hook from an older binary writes a chunk's text and its vector in
//! two statements no transaction of ours can fence (astra r2 #3, #6). And a
//! lock file ([`MaintenanceLock`]) for the whole run, which every writer of
//! this binary honours: hook imports skip (the watcher picks the file up later,
//! its import state untouched), the watcher sleeps, `store_reflection` waits.
//! A lock whose process is gone is ignored by every reader.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};

/// The lock file, in the data directory (the database's directory).
pub const LOCK_FILE: &str = "maintenance.lock";

/// How long the watcher sleeps between looks at a held lock.
pub const POLL: Duration = Duration::from_secs(2);

/// The maintenance run a lock file names.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MaintenanceHolder {
    pub pid: u32,
    pub started_at: String,
}

fn read(data_dir: &Path) -> Option<MaintenanceHolder> {
    let raw = std::fs::read_to_string(data_dir.join(LOCK_FILE)).ok()?;
    let mut pid = None;
    let mut started_at = String::new();
    for line in raw.lines() {
        match line.split_once('=') {
            Some(("pid", value)) => pid = value.trim().parse().ok(),
            Some(("started_at", value)) => started_at = value.trim().to_string(),
            _ => {}
        }
    }
    Some(MaintenanceHolder {
        pid: pid?,
        started_at,
    })
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks that the process exists and may be signalled.
    let sent = unsafe { libc::kill(pid, 0) };
    sent == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    true
}

/// The maintenance run in progress for `data_dir`, if any: a lock file naming a
/// live process other than this one. A stale lock (its process gone) is none.
pub fn active(data_dir: &Path) -> Option<MaintenanceHolder> {
    read(data_dir).filter(|holder| holder.pid != std::process::id() && pid_alive(holder.pid))
}

/// Held for a maintenance run's whole length; the file goes when this drops
/// (an error or a panic included). A signal handler calls [`Self::release`].
#[derive(Debug)]
pub struct MaintenanceLock {
    path: PathBuf,
}

impl MaintenanceLock {
    /// Take the lock in `data_dir`, replacing a stale one. Fails while another
    /// live maintenance run holds it.
    pub fn acquire(data_dir: &Path) -> Result<Self> {
        if let Some(holder) = active(data_dir) {
            anyhow::bail!(
                "another maintenance run (pid {}) has held {} since {}",
                holder.pid,
                data_dir.join(LOCK_FILE).display(),
                holder.started_at
            );
        }
        std::fs::create_dir_all(data_dir)?;
        let path = data_dir.join(LOCK_FILE);
        let tmp = data_dir.join(format!("{LOCK_FILE}.{}", std::process::id()));
        std::fs::write(
            &tmp,
            format!(
                "pid={}\nstarted_at={}\n",
                std::process::id(),
                chrono::Utc::now().to_rfc3339()
            ),
        )
        .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Remove the lock now, if it is still this process's.
    pub fn release(path: &Path) {
        let ours = path
            .parent()
            .and_then(read)
            .is_some_and(|holder| holder.pid == std::process::id());
        if ours {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl Drop for MaintenanceLock {
    fn drop(&mut self) {
        Self::release(&self.path);
    }
}

/// Wait up to `limit` for a maintenance run in `data_dir` to end. An error says
/// so, for a caller that must not write meanwhile and cannot queue the write.
pub async fn wait_until_clear(data_dir: &Path, limit: Duration) -> Result<()> {
    let deadline = tokio::time::Instant::now() + limit;
    while let Some(holder) = active(data_dir) {
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "maintenance in progress (pid {}, since {}): retry when it ends",
                holder.pid,
                holder.started_at
            );
        }
        tokio::time::sleep(Duration::from_millis(250).min(limit)).await;
    }
    Ok(())
}

/// Sleep while a maintenance run in `data_dir` is in progress.
pub async fn sleep_while_active(data_dir: &Path) {
    let mut said = false;
    while let Some(holder) = active(data_dir) {
        if !said {
            tracing::info!(pid = holder.pid, "maintenance in progress; import paused");
            said = true;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// A process other than this one with the database open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbHolder {
    pub pid: u32,
    pub executable: String,
}

/// Every process other than this one holding `db` or its `-wal` open, per
/// `lsof`. An error when `lsof` cannot run: nothing can then be verified.
pub fn other_holders(db: &Path) -> Result<Vec<DbHolder>> {
    let wal = PathBuf::from(format!("{}-wal", db.display()));
    let files = [db.to_path_buf(), wal]
        .into_iter()
        .filter(|path| path.exists())
        .collect::<Vec<_>>();
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let output = std::process::Command::new("lsof")
        .arg("-t")
        .arg("--")
        .args(&files)
        .output()
        .context("running lsof to find other processes holding the database")?;
    // lsof exits 1 with no output when no process has the files open.
    if !output.status.success() && !output.stdout.is_empty() {
        anyhow::bail!(
            "lsof failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let mut pids = String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .filter_map(|pid| pid.parse::<u32>().ok())
        .filter(|&pid| pid != std::process::id())
        .collect::<Vec<_>>();
    pids.sort_unstable();
    pids.dedup();
    Ok(pids
        .into_iter()
        .map(|pid| DbHolder {
            pid,
            executable: executable_of(pid),
        })
        .collect())
}

fn executable_of(pid: u32) -> String {
    if let Ok(path) = std::fs::read_link(format!("/proc/{pid}/exe")) {
        return path.display().to_string();
    }
    std::process::Command::new("ps")
        .args(["-o", "comm=", "-p", &pid.to_string()])
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lock_names_its_run_and_goes_when_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let lock = MaintenanceLock::acquire(dir.path()).unwrap();
        assert!(lock.path().exists());
        let holder = read(dir.path()).unwrap();
        assert_eq!(holder.pid, std::process::id());
        // This process's own lock never blocks it.
        assert!(active(dir.path()).is_none());
        drop(lock);
        assert!(!dir.path().join(LOCK_FILE).exists());
    }

    #[test]
    fn a_lock_of_a_dead_process_is_ignored_and_replaced() {
        let dir = tempfile::tempdir().unwrap();
        // A pid no live process has (pid_max is far below this on macOS and Linux).
        std::fs::write(
            dir.path().join(LOCK_FILE),
            "pid=99999999\nstarted_at=2026-10-12T00:00:00Z\n",
        )
        .unwrap();
        assert!(active(dir.path()).is_none());
        let lock = MaintenanceLock::acquire(dir.path()).unwrap();
        assert_eq!(read(dir.path()).unwrap().pid, std::process::id());
        drop(lock);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_live_lock_blocks_writers_until_it_goes() {
        let dir = tempfile::tempdir().unwrap();
        // Another live process: this test's parent, which outlives the test.
        let parent = std::os::unix::process::parent_id();
        std::fs::write(
            dir.path().join(LOCK_FILE),
            format!("pid={parent}\nstarted_at=2026-10-12T00:00:00Z\n"),
        )
        .unwrap();
        assert_eq!(active(dir.path()).map(|holder| holder.pid), Some(parent));
        assert!(MaintenanceLock::acquire(dir.path()).is_err());
        let error = wait_until_clear(dir.path(), Duration::from_millis(300))
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("maintenance in progress"),
            "{error}"
        );
        let path = dir.path().join(LOCK_FILE);
        let remover = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            std::fs::remove_file(path).unwrap();
        });
        wait_until_clear(dir.path(), Duration::from_secs(5))
            .await
            .unwrap();
        remover.await.unwrap();
    }

    #[test]
    fn the_preflight_lists_another_process_holding_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("csr-engine.db");
        std::fs::write(&db, b"").unwrap();
        assert!(other_holders(&db).unwrap().is_empty());
        let mut holder = std::process::Command::new("tail")
            .arg("-f")
            .arg(&db)
            .spawn()
            .unwrap();
        // lsof sees the child once it has opened the file.
        let mut seen = Vec::new();
        for _ in 0..50 {
            seen = other_holders(&db).unwrap();
            if !seen.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        holder.kill().unwrap();
        holder.wait().unwrap();
        assert_eq!(
            seen.iter().map(|h| h.pid).collect::<Vec<_>>(),
            vec![holder.id()]
        );
        assert!(seen[0].executable.contains("tail"), "{:?}", seen[0]);
    }
}
