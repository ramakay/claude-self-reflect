//! OS-held exclusion between maintenance and database writers or index publishers.
//! The persistent file records status information; only its file lock owns the run.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
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

// Weak entries never extend an operation's lifetime. A nested call on this
// thread borrows the existing ownership, including across Storage calls.
thread_local! {
    static OPERATIONS: RefCell<HashMap<PathBuf, (Weak<File>, PathBuf)>> = RefCell::new(HashMap::new());
}

/// Try once without blocking. A busy exclusive lock returns no guard.
pub fn try_shared(data_dir: &Path) -> Result<Option<SharedLock>> {
    if let Some(lock) = OPERATIONS.with(|operations| {
        let operations = operations.borrow();
        let (file, canonical) = operations.get(data_dir)?;
        Some(SharedLock {
            _file: file.upgrade()?,
            data_dir: canonical.clone(),
        })
    }) {
        return Ok(Some(lock));
    }
    let file = open(data_dir)?;
    match FileExt::try_lock_shared(&file) {
        Ok(()) => {
            let lock = SharedLock {
                _file: Arc::new(file),
                data_dir: data_dir
                    .canonicalize()
                    .context("resolving maintenance directory")?,
            };
            OPERATIONS.with(|operations| {
                let mut operations = operations.borrow_mut();
                operations.retain(|_, (file, _)| file.strong_count() > 0);
                operations.insert(
                    data_dir.to_path_buf(),
                    (Arc::downgrade(&lock._file), lock.data_dir.clone()),
                );
                operations.insert(
                    lock.data_dir.clone(),
                    (Arc::downgrade(&lock._file), lock.data_dir.clone()),
                );
            });
            Ok(Some(lock))
        }
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
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut command = std::process::Command::new("lsof");
    command.arg("-t").arg("--").args(&files);
    let output = command_output(
        &mut command,
        deadline.saturating_duration_since(Instant::now()),
    )?;
    let mut pids = parse_lsof(&output)?;
    pids.retain(|&pid| pid != std::process::id());
    pids.sort_unstable();
    pids.dedup();
    Ok(pids
        .into_iter()
        .map(|pid| DbHolder {
            pid,
            executable: executable_of(pid, deadline),
        })
        .collect())
}

fn executable_of(pid: u32, deadline: Instant) -> String {
    if let Ok(path) = std::fs::read_link(format!("/proc/{pid}/exe")) {
        return path.display().to_string();
    }
    let mut command = std::process::Command::new("ps");
    command.args(["-o", "comm=", "-p", &pid.to_string()]);
    executable_from_command(&mut command, deadline)
}

fn executable_from_command(command: &mut std::process::Command, deadline: Instant) -> String {
    command_output(command, deadline.saturating_duration_since(Instant::now()))
        .ok()
        .filter(|output| output.status.success())
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
    anyhow::ensure!(!limit.is_zero(), "process lookup deadline expired");
    let deadline = Instant::now() + limit;
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
    fn round4_nested_shared_reuses_operation_handle() {
        let dir = tempfile::tempdir().unwrap();
        let storage = crate::storage::Storage::open(&dir.path().join("db")).unwrap();
        let first = shared(dir.path()).unwrap();
        let second = storage.operation_lock().unwrap().unwrap();
        assert!(Arc::ptr_eq(&first._file, &second._file));
        drop(first);
        assert!(MaintenanceLock::acquire_for(dir.path(), Duration::ZERO).is_err());
        drop(second);
        // Concurrent process tests can briefly inherit a descriptor before exec.
        assert!(MaintenanceLock::acquire_for(dir.path(), Duration::from_secs(1)).is_ok());
        drop(storage);
    }

    #[test]
    fn a_lock_names_its_run_and_goes_when_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let lock = MaintenanceLock::acquire(dir.path()).unwrap();
        assert!(lock.path().exists());
        assert_eq!(read(dir.path()).unwrap().pid, std::process::id());
        assert_eq!(active(dir.path()).unwrap().pid, std::process::id());
        assert!(try_shared(dir.path()).unwrap().is_none());
        drop(lock);
        // Ownership goes away; the persistent inode must never be removed.
        assert!(dir.path().join(LOCK_FILE).exists());
        assert!(active(dir.path()).is_none());
        assert!(try_shared(dir.path()).unwrap().is_some());
    }

    #[test]
    fn a_lock_of_a_dead_process_is_ignored_and_replaced() {
        let dir = tempfile::tempdir().unwrap();
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

    #[tokio::test]
    async fn a_live_lock_blocks_writers_until_it_goes() {
        let dir = tempfile::tempdir().unwrap();
        // An OS-held lock, rather than PID metadata, proves live ownership.
        let owner = MaintenanceLock::acquire(dir.path()).unwrap();
        assert!(MaintenanceLock::acquire_for(dir.path(), Duration::ZERO).is_err());
        let error = wait_shared(dir.path(), Duration::from_millis(30))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("maintenance in progress"));
        let releaser = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(owner);
        });
        let writer = wait_shared(dir.path(), Duration::from_secs(5))
            .await
            .unwrap();
        releaser.await.unwrap();
        assert!(MaintenanceLock::acquire_for(dir.path(), Duration::ZERO).is_err());
        drop(writer);
        assert!(MaintenanceLock::acquire_for(dir.path(), Duration::ZERO).is_ok());
    }

    #[test]
    fn round3_sixteen_exclusive_attempts_have_one_owner() {
        let dir = tempfile::tempdir().unwrap();
        for trial in 0..10 {
            let run = dir.path().join(trial.to_string());
            std::fs::create_dir(&run).unwrap();
            let mut children = (0..16)
                .map(|id| {
                    std::process::Command::new(std::env::current_exe().unwrap())
                        .args([
                            "--exact",
                            "maintenance::tests::round4_exclusivity_child_entry",
                        ])
                        .env("CSR_R4_RACE_DIR", &run)
                        .env("CSR_R4_RACE_ID", id.to_string())
                        .stdout(std::process::Stdio::null())
                        .spawn()
                        .unwrap()
                })
                .collect::<Vec<_>>();
            let deadline = Instant::now() + Duration::from_secs(15);
            while !(0..16).all(|id| run.join(format!("ready-{id}")).exists())
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(1));
            }
            std::fs::write(run.join("start"), b"go").unwrap();
            while !(0..16).all(|id| run.join(format!("result-{id}")).exists())
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(1));
            }
            let results = (0..16)
                .map(|id| std::fs::read_to_string(run.join(format!("result-{id}"))).ok())
                .collect::<Vec<_>>();
            std::fs::write(run.join("release"), b"done").unwrap();
            for child in &mut children {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                }
                assert!(child.wait().unwrap().success());
            }
            assert_eq!(
                results
                    .iter()
                    .filter(|v| v.as_deref() == Some("owner"))
                    .count(),
                1,
                "trial {trial}: {results:?}"
            );
            assert_eq!(
                results
                    .iter()
                    .filter(|v| v.as_deref() == Some("refused"))
                    .count(),
                15,
                "trial {trial}: {results:?}"
            );
        }
    }

    #[test]
    fn round4_exclusivity_child_entry() {
        let Some(dir) = std::env::var_os("CSR_R4_RACE_DIR") else {
            return;
        };
        let dir = PathBuf::from(dir);
        let id = std::env::var("CSR_R4_RACE_ID").unwrap();
        std::fs::write(dir.join(format!("ready-{id}")), b"ready").unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while !dir.join("start").exists() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        let owner = MaintenanceLock::acquire_for(&dir, Duration::ZERO).ok();
        std::fs::write(
            dir.join(format!("result-{id}")),
            if owner.is_some() { "owner" } else { "refused" },
        )
        .unwrap();
        while !dir.join("release").exists() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        drop(owner);
    }

    #[test]
    fn round3_reused_pid_metadata_does_not_own_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOCK_FILE);
        let mut unrelated = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = unrelated.id();
        std::fs::write(
            &path,
            format!("pid={}\nstarted_at=1970-01-01T00:00:00Z\n", pid),
        )
        .unwrap();
        let inactive = active(dir.path()).is_none();
        assert!(unrelated.try_wait().unwrap().is_none());
        let lock = MaintenanceLock::acquire_for(dir.path(), Duration::ZERO);
        unrelated.kill().unwrap();
        unrelated.wait().unwrap();
        assert!(inactive, "live unrelated pid must not own maintenance");
        let lock = lock.unwrap();
        assert_eq!(active(dir.path()).unwrap().pid, std::process::id());
        assert!(try_shared(dir.path()).unwrap().is_none());
        drop(lock);
        assert!(path.exists());
        // A child another test is spawning can hold an inherited copy of the
        // lock descriptor until its exec; wait that window out.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while active(dir.path()).is_some() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
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
        assert!(MaintenanceLock::acquire_for(dir.path(), Duration::from_secs(1)).is_ok());
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

    #[cfg(unix)]
    #[test]
    fn round4_ps_lookup_shares_preflight_deadline() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("db");
        std::fs::write(&db, "").unwrap();
        for (name, body) in [
            ("lsof", "#!/bin/sh\n/bin/sleep 2\necho 4000000000\n"),
            ("ps", "#!/bin/sh\n/bin/sleep 5\necho late\n"),
        ] {
            let path = dir.path().join(name);
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "maintenance::tests::round4_preflight_child_entry",
                "--nocapture",
            ])
            .env("CSR_R4_PREFLIGHT_DB", &db)
            .env("PATH", dir.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn round4_preflight_child_entry() {
        let Some(db) = std::env::var_os("CSR_R4_PREFLIGHT_DB") else {
            return;
        };
        let start = Instant::now();
        let holders = other_holders(Path::new(&db)).unwrap();
        assert!(start.elapsed() < Duration::from_millis(3800));
        assert_eq!(
            holders,
            vec![DbHolder {
                pid: 4_000_000_000,
                executable: "unknown".into()
            }]
        );
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
