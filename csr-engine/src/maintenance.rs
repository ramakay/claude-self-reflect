//! OS-held exclusion between maintenance and database writers or index publishers.
//! The persistent file records status information; only its file lock owns the run.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use fs2::FileExt;

/// The persistent lock file in the database directory.
pub const LOCK_FILE: &str = "maintenance.lock";
const RETRY: Duration = Duration::from_millis(50);

/// Informational metadata for the current maintenance run.
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

fn open(data_dir: &Path) -> Result<File> {
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("creating maintenance directory {}", data_dir.display()))?;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(data_dir.join(LOCK_FILE))
        .with_context(|| format!("opening {}", data_dir.join(LOCK_FILE).display()))
}

/// A shared file lock held for one complete writer or publisher operation.
#[derive(Debug, Clone)]
pub struct SharedLock {
    _file: Arc<File>,
    data_dir: PathBuf,
}

impl SharedLock {
    pub(crate) fn check_directory(&self, data_dir: &Path) -> Result<()> {
        anyhow::ensure!(
            self.data_dir
                == data_dir
                    .canonicalize()
                    .context("resolving publication directory")?,
            "operation lock belongs to another data directory"
        );
        Ok(())
    }
}

/// Try once without blocking. A busy exclusive lock returns no guard.
pub fn try_shared(data_dir: &Path) -> Result<Option<SharedLock>> {
    let file = open(data_dir)?;
    match FileExt::try_lock_shared(&file) {
        Ok(()) => Ok(Some(SharedLock {
            _file: Arc::new(file),
            data_dir: data_dir
                .canonicalize()
                .context("resolving maintenance directory")?,
        })),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error).context("trying shared maintenance lock"),
    }
}

/// Acquire a shared lock immediately or ask the caller to retry later.
pub fn shared(data_dir: &Path) -> Result<SharedLock> {
    try_shared(data_dir)?.context("maintenance in progress, retry when it ends")
}

/// Wait asynchronously using nonblocking attempts, retaining the successful lock.
pub async fn wait_shared(data_dir: &Path, limit: Duration) -> Result<SharedLock> {
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        if let Some(lock) = try_shared(data_dir)? {
            return Ok(lock);
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("maintenance in progress, retry when it ends");
        }
        tokio::time::sleep(
            RETRY.min(deadline.saturating_duration_since(tokio::time::Instant::now())),
        )
        .await;
    }
}

/// Probe the OS lock without blocking. Metadata never determines ownership.
pub fn active(data_dir: &Path) -> Option<MaintenanceHolder> {
    // Status must not create a file or require write access to an idle directory.
    let file = File::open(data_dir.join(LOCK_FILE)).ok()?;
    match FileExt::try_lock_shared(&file) {
        Ok(()) => None,
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
            Some(read(data_dir).unwrap_or(MaintenanceHolder {
                pid: 0,
                started_at: String::new(),
            }))
        }
        Err(_) => None,
    }
}

/// Exclusive ownership of a maintenance run. Clones retain the same OS lock.
/// The file is never removed or renamed, including on errors and process exit.
#[derive(Debug, Clone)]
pub struct MaintenanceLock {
    file: Arc<File>,
    path: PathBuf,
}

impl MaintenanceLock {
    /// Try nonblocking acquisition for at most thirty seconds before refusing.
    pub fn acquire(data_dir: &Path) -> Result<Self> {
        Self::acquire_for(data_dir, Duration::from_secs(30))
    }

    fn acquire_for(data_dir: &Path, limit: Duration) -> Result<Self> {
        let mut file = open(data_dir)?;
        let deadline = Instant::now() + limit;
        loop {
            match FileExt::try_lock_exclusive(&file) {
                Ok(()) => break,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        anyhow::bail!("refusing maintenance: writers or another maintenance run hold {} after {} seconds", data_dir.join(LOCK_FILE).display(), limit.as_secs());
                    }
                    std::thread::sleep(
                        RETRY.min(deadline.saturating_duration_since(Instant::now())),
                    );
                }
                Err(error) => return Err(error).context("trying exclusive maintenance lock"),
            }
        }
        file.set_len(0)
            .context("clearing maintenance status metadata")?;
        write!(
            file,
            "pid={}\nstarted_at={}\n",
            std::process::id(),
            chrono::Utc::now().to_rfc3339()
        )
        .context("writing maintenance status metadata")?;
        Ok(Self {
            file: Arc::new(file),
            path: data_dir
                .canonicalize()
                .context("resolving maintenance directory")?
                .join(LOCK_FILE),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Retain this run's exclusive ownership for an operation in its directory.
    pub(crate) fn operation(&self, data_dir: &Path) -> Result<SharedLock> {
        let dir = data_dir
            .canonicalize()
            .context("resolving operation directory")?;
        anyhow::ensure!(
            self.path.parent() == Some(dir.as_path()),
            "maintenance lock belongs to another data directory"
        );
        Ok(SharedLock {
            _file: self.file.clone(),
            data_dir: dir,
        })
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
    let mut command = std::process::Command::new("lsof");
    command.arg("-t").arg("--").args(&files);
    let output = command_output(&mut command, Duration::from_secs(3))?;
    let mut pids = parse_lsof(&output)?;
    pids.retain(|&pid| pid != std::process::id());
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

/// Collect both pipes while enforcing a deadline on the process and its output.
fn command_output(
    command: &mut std::process::Command,
    limit: Duration,
) -> Result<std::process::Output> {
    use std::io::Read;
    use std::process::Stdio;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("running lsof to find other processes holding the database")?;
    let stdout = child.stdout.take().context("opening lsof stdout")?;
    let stderr = child.stderr.take().context("opening lsof stderr")?;
    fn reader(
        pipe: impl Read + Send + 'static,
    ) -> std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>> {
        let (send, recv) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = pipe
                .take(1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .and_then(|_| {
                    if bytes.len() > 1024 * 1024 {
                        Err(std::io::Error::other(
                            "lsof output exceeded the enumeration limit",
                        ))
                    } else {
                        Ok(bytes)
                    }
                });
            let _ = send.send(result);
        });
        recv
    }
    let stdout = reader(stdout);
    let stderr = reader(stderr);
    let deadline = Instant::now() + limit;
    let mut status = None;
    let mut out = None;
    let mut err = None;
    loop {
        if status.is_none() {
            status = child.try_wait().context("waiting for lsof")?;
        }
        if out.is_none() {
            out = stdout.try_recv().ok();
        }
        if err.is_none() {
            err = stderr.try_recv().ok();
        }
        if status.is_some() && out.is_some() && err.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            #[cfg(unix)]
            // SAFETY: the child was started in its own process group above.
            unsafe {
                libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!(
                "lsof timed out after {} seconds; holder enumeration is incomplete",
                limit.as_secs_f64()
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(std::process::Output {
        status: status.context("missing lsof exit status")?,
        stdout: out
            .context("missing lsof stdout")?
            .context("reading lsof stdout")?,
        stderr: err
            .context("missing lsof stderr")?
            .context("reading lsof stderr")?,
    })
}

fn parse_lsof(output: &std::process::Output) -> Result<Vec<u32>> {
    anyhow::ensure!(
        output.stderr.is_empty(),
        "lsof reported an error: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    if output.status.code() == Some(1) && output.stdout.is_empty() {
        return Ok(Vec::new());
    }
    anyhow::ensure!(
        output.status.success(),
        "lsof failed with {}",
        output.status
    );
    let text = std::str::from_utf8(&output.stdout).context("lsof returned non-UTF-8 output")?;
    anyhow::ensure!(
        !text.trim().is_empty(),
        "lsof succeeded without valid holder output"
    );
    text.lines()
        .map(|line| {
            let pid = line
                .parse::<u32>()
                .context("lsof returned an invalid process id")?;
            anyhow::ensure!(pid > 0, "lsof returned a zero process id");
            Ok(pid)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round3_sixteen_exclusive_attempts_have_one_owner() {
        let dir = tempfile::tempdir().unwrap();
        let start = Arc::new(std::sync::Barrier::new(16));
        let finish = Arc::new(std::sync::Barrier::new(16));
        let owners = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let workers = (0..16)
            .map(|_| {
                let path = dir.path().to_path_buf();
                let start = start.clone();
                let finish = finish.clone();
                let owners = owners.clone();
                std::thread::spawn(move || {
                    start.wait();
                    let lock = MaintenanceLock::acquire_for(&path, Duration::ZERO).ok();
                    if lock.is_some() {
                        owners.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                    finish.wait();
                    drop(lock);
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(owners.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn round3_reused_pid_metadata_does_not_own_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOCK_FILE);
        std::fs::write(
            &path,
            format!(
                "pid={}\nstarted_at=1970-01-01T00:00:00Z\n",
                std::process::id()
            ),
        )
        .unwrap();
        assert!(active(dir.path()).is_none());
        let lock = MaintenanceLock::acquire_for(dir.path(), Duration::ZERO).unwrap();
        assert_eq!(active(dir.path()).unwrap().pid, std::process::id());
        assert!(try_shared(dir.path()).unwrap().is_none());
        drop(lock);
        assert!(path.exists());
        assert!(active(dir.path()).is_none());
        assert!(try_shared(dir.path()).unwrap().is_some());
    }

    #[test]
    fn shared_operations_exclude_maintenance_until_every_handle_drops() {
        let dir = tempfile::tempdir().unwrap();
        let first = shared(dir.path()).unwrap();
        let second = shared(dir.path()).unwrap();
        assert!(MaintenanceLock::acquire_for(dir.path(), Duration::ZERO).is_err());
        drop(first);
        assert!(MaintenanceLock::acquire_for(dir.path(), Duration::ZERO).is_err());
        drop(second);
        assert!(MaintenanceLock::acquire_for(dir.path(), Duration::ZERO).is_ok());
    }

    #[tokio::test]
    async fn reflection_wait_retains_the_acquired_shared_lock() {
        let dir = tempfile::tempdir().unwrap();
        let owner = MaintenanceLock::acquire(dir.path()).unwrap();
        let error = wait_shared(dir.path(), Duration::from_millis(30))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("maintenance in progress"));
        drop(owner);
        let writer = wait_shared(dir.path(), Duration::from_secs(10))
            .await
            .unwrap();
        assert!(MaintenanceLock::acquire_for(dir.path(), Duration::ZERO).is_err());
        drop(writer);
        assert!(MaintenanceLock::acquire_for(dir.path(), Duration::ZERO).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn round3_lsof_errors_and_malformed_output_refuse_preflight() {
        use std::os::unix::process::ExitStatusExt;
        for code in [1, 2, 127] {
            let output = std::process::Output {
                status: std::process::ExitStatus::from_raw(code << 8),
                stdout: Vec::new(),
                stderr: b"lsof: forced failure\n".to_vec(),
            };
            assert!(parse_lsof(&output).is_err(), "exit {code}");
        }
        for (code, stdout, stderr, accepted) in [
            (1, "", "", true),
            (2, "", "", false),
            (127, "", "", false),
            (0, "123\n456\n", "", true),
            (0, "bogus\n", "", false),
            (0, "0\n", "", false),
            (0, "123\n", "warning\n", false),
        ] {
            let output = std::process::Output {
                status: std::process::ExitStatus::from_raw(code << 8),
                stdout: stdout.as_bytes().to_vec(),
                stderr: stderr.as_bytes().to_vec(),
            };
            assert_eq!(
                parse_lsof(&output).is_ok(),
                accepted,
                "exit {code}, {stdout:?}, {stderr:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn round3_lsof_four_second_stub_is_killed_at_deadline() {
        let mut command = std::process::Command::new("/bin/sh");
        command.args(["-c", "/bin/sleep 4; exit 1"]);
        let start = Instant::now();
        let error = command_output(&mut command, Duration::from_secs(3)).unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(start.elapsed() < Duration::from_millis(3800));
    }

    #[cfg(unix)]
    #[test]
    fn round3_lsof_stderr_stub_cannot_masquerade_as_no_match() {
        for code in [1, 2, 127] {
            let mut command = std::process::Command::new("/bin/sh");
            command.args([
                "-c",
                &format!("echo 'lsof: forced failure' >&2; exit {code}"),
            ]);
            assert!(
                parse_lsof(&command_output(&mut command, Duration::from_secs(3)).unwrap()).is_err()
            );
        }
    }

    #[test]
    fn maintenance_child_entry() {
        let Some(path) = std::env::var_os("CSR_R3_LOCK_CHILD") else {
            return;
        };
        let path = PathBuf::from(path);
        let _owner = MaintenanceLock::acquire(&path).unwrap();
        std::fs::write(path.join("ready"), b"ready").unwrap();
        std::thread::sleep(Duration::from_secs(60));
    }

    #[test]
    fn os_releases_maintenance_after_process_death() {
        let dir = tempfile::tempdir().unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "maintenance::tests::maintenance_child_entry"])
            .env("CSR_R3_LOCK_CHILD", dir.path())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !dir.path().join("ready").exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let held = active(dir.path()).is_some();
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(held);
        assert!(dir.path().join(LOCK_FILE).exists());
        assert!(active(dir.path()).is_none());
        assert!(MaintenanceLock::acquire_for(dir.path(), Duration::ZERO).is_ok());
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
