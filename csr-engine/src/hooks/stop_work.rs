//! The Stop hook returns at once; the transcript work runs in a detached worker.
//!
//! `hook stop` used to import the transcript, extract the episode and write the
//! rolling summary before it returned, and Claude Code waited for all of it: two
//! to three full passes over a transcript that reaches hundreds of MB (measured
//! 2026-10-20: 8.9 s per Stop on a 163.5 MB transcript). Nothing Claude Code
//! consumes comes back from Stop (it prints nothing to stdout), so the wait buys
//! nothing.
//!
//! Now `hook stop` reads stdin, writes the input to a spool file and starts
//! `csr-engine hook stop-work --session <key>` detached, then exits:
//!
//! ```text
//! <data dir>/stop-work/<key>.json   the latest Stop input for the session (0600)
//! <data dir>/stop-work/<key>.lock   flock; the holder is the session's one worker
//! <data dir>/stop-work/<key>.work   the input the holder is processing right now
//! ```
//!
//! The worker runs [`super::stop::handle`] unchanged on the spooled input.
//! Coalescing: a worker that cannot take the lock leaves the spool and exits (the
//! holder will find it); the holder claims the spool by renaming it to `.work`,
//! runs it, and claims again until no spool is left, so the last Stop's input is
//! always processed and a burst of Stops costs at most one run per input that
//! arrived while a run was in flight. After releasing the lock the holder looks
//! once more: a spool written while it still held the lock (its own worker having
//! failed the lock) is picked up by taking the lock again.
//!
//! `CSR_STOP_INLINE=1` keeps the old inline behaviour, and so does any failure to
//! spool or to start the worker.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::HookInput;
use crate::search::cross_project::resolve_project_from_cwd;

/// Folder under the data dir holding the spools and locks.
pub const SPOOL_DIR: &str = "stop-work";

/// A spool older than this with no worker is picked up by the next worker that
/// starts for any session.
const STALE_AFTER: Duration = Duration::from_secs(24 * 3600);

/// Stale spools one worker takes on besides its own session's.
const STALE_PER_WORKER: usize = 2;

/// What the spool file holds.
#[derive(Debug, Serialize, Deserialize)]
pub struct Spool {
    /// Unique per write (nanoseconds and pid), so two inputs never compare equal.
    pub seq: String,
    pub input: HookInput,
}

/// `CSR_STOP_INLINE=1`: run the Stop handler inside the hook, as before.
pub fn inline_requested() -> bool {
    std::env::var("CSR_STOP_INLINE").as_deref() == Ok("1")
}

/// The spool folder for a database path (`<data dir>/stop-work`).
pub fn spool_dir(db_path: &Path) -> PathBuf {
    db_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .join(SPOOL_DIR)
}

fn safe(part: &str) -> String {
    part.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(120)
        .collect()
}

/// The coalescing key of a Stop input: the session id, plus the agent id for a
/// hook that fired inside a subagent (its session id is the parent's, but its
/// transcript is not, so it must not replace the parent's spool). `None` when
/// there is no usable session id.
pub fn spool_key(input: &HookInput) -> Option<String> {
    let session = safe(input.session_id.as_deref()?);
    if session.is_empty() {
        return None;
    }
    match input
        .agent_id
        .as_deref()
        .map(safe)
        .filter(|a| !a.is_empty())
    {
        Some(agent) => Some(format!("{session}.{agent}")),
        None => Some(session),
    }
}

fn spool_path(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.json"))
}

fn lock_path(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.lock"))
}

fn work_path(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.work"))
}

#[cfg(unix)]
fn private_options(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn private_options(_options: &mut OpenOptions) {}

fn make_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

/// Write `input` as the session's spool: a private temp file, then a rename, so a
/// reader never sees half a file and the newest write wins. Returns the `seq`.
pub fn write_spool(dir: &Path, key: &str, input: &HookInput) -> io::Result<String> {
    make_dir(dir)?;
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let seq = format!("{nanos}-{}", std::process::id());
    let body = serde_json::to_vec(&Spool {
        seq: seq.clone(),
        input: input.clone(),
    })
    .map_err(io::Error::other)?;
    let tmp = dir.join(format!(".{key}.{seq}.tmp"));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    private_options(&mut options);
    let written = options
        .open(&tmp)
        .and_then(|mut f| io::Write::write_all(&mut f, &body))
        .and_then(|()| std::fs::rename(&tmp, spool_path(dir, key)));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written.map(|()| seq)
}

fn read_spool(path: &Path) -> Option<Spool> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Whether a spool (or a half-processed claim of one) waits for a worker.
pub fn has_pending(dir: &Path, key: &str) -> bool {
    spool_path(dir, key).exists() || work_path(dir, key).exists()
}

/// Starts the detached worker. A trait so tests can fail the spawn.
pub trait WorkerSpawner {
    fn spawn(&self, argv: &[OsString]) -> io::Result<()>;
}

/// The real spawner: this binary again, null stdio, its own process group (it
/// outlives the hook when Claude Code reaps it), never waited on.
/// `CSR_STOP_WORK_EXE` names another binary; a test seam for the failure path.
pub struct DetachedSpawner;

impl WorkerSpawner for DetachedSpawner {
    // The hook exits right after; init reaps the worker. Waiting would hold the
    // hook until the work ends.
    #[allow(clippy::zombie_processes)]
    fn spawn(&self, argv: &[OsString]) -> io::Result<()> {
        let exe = match std::env::var_os("CSR_STOP_WORK_EXE") {
            Some(exe) if !exe.is_empty() => PathBuf::from(exe),
            _ => std::env::current_exe()?,
        };
        let mut cmd = std::process::Command::new(exe);
        cmd.args(argv)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        cmd.spawn().map(|_| ())
    }
}

fn worker_argv(db_path: &Path, projects_dir: &Path, key: &str) -> Vec<OsString> {
    vec![
        "--db-path".into(),
        db_path.as_os_str().to_os_string(),
        "--projects-dir".into(),
        projects_dir.as_os_str().to_os_string(),
        "hook".into(),
        "stop-work".into(),
        "--session".into(),
        key.into(),
    ]
}

/// Spool this Stop and start its worker. `true`: the work is handed off and the
/// hook may exit. `false`: the caller runs the handler inline (kill switch, no
/// session id, spool or spawn failure), so no Stop is lost.
pub fn detach_stop_with(
    db_path: &Path,
    projects_dir: &Path,
    input: &HookInput,
    spawner: &dyn WorkerSpawner,
) -> bool {
    if inline_requested() {
        return false;
    }
    let Some(key) = spool_key(input) else {
        return false;
    };
    let dir = spool_dir(db_path);
    let seq = match write_spool(&dir, &key, input) {
        Ok(seq) => seq,
        Err(e) => {
            eprintln!("CSR: stop spool failed, running inline: {e}");
            return false;
        }
    };
    if let Err(e) = spawner.spawn(&worker_argv(db_path, projects_dir, &key)) {
        eprintln!("CSR: stop worker did not start, running inline: {e}");
        // This input is about to run inline; leave no copy of it behind unless a
        // newer Stop has replaced it already.
        let path = spool_path(&dir, &key);
        if read_spool(&path).is_some_and(|s| s.seq == seq) {
            let _ = std::fs::remove_file(path);
        }
        return false;
    }
    true
}

/// The fast path of `hook stop`; logs the hook-timing line the inline path would.
pub fn detach_stop(
    db_path: &Path,
    projects_dir: &Path,
    input: &HookInput,
    stdin_elapsed: Duration,
) -> bool {
    let started = Instant::now();
    let detached = detach_stop_with(db_path, projects_dir, input, &DetachedSpawner);
    if detached {
        let project = input
            .cwd
            .as_deref()
            .and_then(resolve_project_from_cwd)
            .unwrap_or_else(|| "unknown".to_string());
        let hook = started.elapsed();
        crate::telemetry::append_timing_line(&format!(
            "CSR hook stop [{project}]: stdin={}ms setup=0ms hook={}ms flush=0ms total={}ms",
            stdin_elapsed.as_millis(),
            hook.as_millis(),
            (stdin_elapsed + hook).as_millis(),
        ));
    }
    detached
}

/// The session's worker lock. Held for as long as the value lives.
pub struct KeyLock {
    _file: File,
    dir: PathBuf,
    key: String,
}

impl KeyLock {
    /// `None` when another worker holds the lock (it will find the spool) or the
    /// lock file cannot be opened.
    pub fn try_acquire(dir: &Path, key: &str) -> Option<Self> {
        make_dir(dir).ok()?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        private_options(&mut options);
        let file = options.open(lock_path(dir, key)).ok()?;
        fs2::FileExt::try_lock_exclusive(&file).ok()?;
        Some(Self {
            _file: file,
            dir: dir.to_path_buf(),
            key: key.to_string(),
        })
    }

    /// Claim the next input: the leftover of a worker that died mid-run, unless a
    /// newer spool has replaced it, else the spool itself, renamed to `.work` so
    /// a Stop that lands during the run writes a fresh spool. An unreadable claim
    /// is dropped. `None` when nothing is waiting.
    pub fn claim_next(&self) -> Option<HookInput> {
        let spool = spool_path(&self.dir, &self.key);
        let work = work_path(&self.dir, &self.key);
        loop {
            if work.exists() && spool.exists() {
                let _ = std::fs::remove_file(&work);
            }
            if !work.exists() {
                std::fs::rename(&spool, &work).ok()?;
            }
            match read_spool(&work) {
                Some(claimed) => return Some(claimed.input),
                None => {
                    let _ = std::fs::remove_file(&work);
                }
            }
        }
    }

    /// The claimed input is processed.
    pub fn finish(&self) {
        let _ = std::fs::remove_file(work_path(&self.dir, &self.key));
    }
}

/// Keys with a spool older than [`STALE_AFTER`] and no worker on them.
fn stale_keys(dir: &Path, exclude: &str, now: SystemTime) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut keys: Vec<(SystemTime, String)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let key = name.strip_suffix(".json")?.to_string();
            if key.starts_with('.') || key == exclude {
                return None;
            }
            let modified = entry.metadata().ok()?.modified().ok()?;
            let age = now.duration_since(modified).unwrap_or_default();
            (age >= STALE_AFTER).then_some((modified, key))
        })
        .collect();
    keys.sort();
    keys.into_iter()
        .map(|(_, key)| key)
        .take(STALE_PER_WORKER)
        .collect()
}

/// Engine and log label shared by the runs of one worker.
struct Runner<'a> {
    db_path: &'a Path,
    projects_dir: &'a Path,
    engine: Option<crate::engine::Engine>,
    project: Option<String>,
}

impl Runner<'_> {
    /// The import-only engine every Stop uses, built on first need.
    fn engine(&mut self) -> Result<&crate::engine::Engine> {
        if self.engine.is_none() {
            let engine = match crate::engine::Engine::new_proxied(self.db_path, self.projects_dir)?
            {
                Some(proxied) => proxied,
                None => crate::engine::Engine::new_import_only(self.db_path, self.projects_dir)?,
            };
            self.engine = Some(engine);
        }
        Ok(self.engine.as_ref().expect("engine just built"))
    }

    /// Drain one key. Returns how many inputs were processed.
    async fn drain(&mut self, dir: &Path, key: &str) -> Result<usize> {
        let mut runs = 0;
        loop {
            let Some(lock) = KeyLock::try_acquire(dir, key) else {
                break;
            };
            if !has_pending(dir, key) {
                break;
            }
            self.engine()?;
            let mut pass = 0;
            while let Some(input) = lock.claim_next() {
                self.run_one(&input).await;
                lock.finish();
                pass += 1;
            }
            runs += pass;
            drop(lock);
            // A Stop that landed while the lock was held had its worker turned
            // away; take the lock again to run it. No progress ends the loop.
            if pass == 0 || !has_pending(dir, key) {
                break;
            }
        }
        Ok(runs)
    }

    /// Today's Stop handler, on a spooled input.
    async fn run_one(&mut self, input: &HookInput) {
        let Some(engine) = self.engine.as_ref() else {
            return;
        };
        let Ok(_operation) = engine.storage().operation_lock() else {
            return;
        };
        let cwd = super::resolve_hook_cwd(input.cwd.as_deref());
        if self.project.is_none() {
            self.project = resolve_project_from_cwd(&cwd.to_string_lossy());
        }
        if let Err(e) = super::stop::handle(input, engine, &cwd).await {
            eprintln!("CSR: stop worker error (non-fatal): {e}");
        }
    }
}

/// `hook stop-work --session <key>`: process the session's spooled Stop input,
/// then any spool that has gone stale. Logs one `stop-work` timing line when it
/// ran something.
pub async fn worker_main(db_path: &Path, projects_dir: &Path, key: &str) -> Result<()> {
    let key = safe(key);
    if key.is_empty() {
        return Ok(());
    }
    let started = Instant::now();
    let dir = spool_dir(db_path);
    let mut runner = Runner {
        db_path,
        projects_dir,
        engine: None,
        project: None,
    };
    let mut runs = runner.drain(&dir, &key).await?;
    for stale in stale_keys(&dir, &key, SystemTime::now()) {
        runs += runner.drain(&dir, &stale).await?;
    }
    if runs > 0 {
        let project = runner.project.as_deref().unwrap_or("unknown");
        crate::telemetry::append_timing_line(&format!(
            "CSR hook stop-work [{project}]: total={}ms runs={runs}",
            started.elapsed().as_millis(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(session: &str, transcript: &str) -> HookInput {
        HookInput {
            session_id: Some(session.to_string()),
            transcript_path: Some(transcript.to_string()),
            cwd: Some("/tmp/proj".to_string()),
            ..Default::default()
        }
    }

    struct Recording(std::sync::Mutex<Vec<Vec<OsString>>>);
    impl WorkerSpawner for Recording {
        fn spawn(&self, argv: &[OsString]) -> io::Result<()> {
            self.0.lock().unwrap().push(argv.to_vec());
            Ok(())
        }
    }

    struct Failing;
    impl WorkerSpawner for Failing {
        fn spawn(&self, _: &[OsString]) -> io::Result<()> {
            Err(io::Error::new(io::ErrorKind::NotFound, "no such binary"))
        }
    }

    fn inline_off() {
        // Every test here runs with the kill switch unset; the integration tests
        // that set it do so on a child process, never on this one.
        assert!(
            !inline_requested(),
            "CSR_STOP_INLINE leaked into the unit tests"
        );
    }

    #[test]
    fn spool_key_is_the_session_and_a_subagent_gets_its_own() {
        let main = input("abc-123", "/t.jsonl");
        assert_eq!(spool_key(&main).as_deref(), Some("abc-123"));
        let mut sub = main.clone();
        sub.agent_id = Some("a9f".into());
        assert_eq!(spool_key(&sub).as_deref(), Some("abc-123.a9f"));
        let mut none = main.clone();
        none.session_id = None;
        assert_eq!(spool_key(&none), None);
        let mut hostile = main.clone();
        hostile.session_id = Some("../../etc/x".into());
        assert_eq!(spool_key(&hostile).as_deref(), Some("etcx"));
        hostile.session_id = Some("../..".into());
        assert_eq!(spool_key(&hostile), None);
    }

    #[test]
    fn detach_spools_the_input_privately_and_starts_one_worker() {
        inline_off();
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("csr-engine.db");
        let projects = tmp.path().join("projects");
        let spawner = Recording(Default::default());
        let sent = input("sid1", "/t/one.jsonl");
        assert!(detach_stop_with(&db, &projects, &sent, &spawner));

        let spool = spool_dir(&db).join("sid1.json");
        let stored = read_spool(&spool).expect("spool written");
        assert_eq!(
            stored.input.transcript_path.as_deref(),
            Some("/t/one.jsonl")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&spool).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "spool must be private");
        }
        let calls = spawner.0.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let argv: Vec<String> = calls[0]
            .iter()
            .map(|a| a.to_string_lossy().into())
            .collect();
        assert_eq!(&argv[4..], ["hook", "stop-work", "--session", "sid1"]);
        let leftovers: Vec<_> = std::fs::read_dir(spool_dir(&db))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftovers, ["sid1.json"], "no temp file left behind");
    }

    #[test]
    fn a_failed_spawn_runs_inline_and_leaves_no_spool() {
        inline_off();
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("csr-engine.db");
        let sent = input("sid2", "/t/two.jsonl");
        assert!(!detach_stop_with(&db, tmp.path(), &sent, &Failing));
        assert!(!spool_dir(&db).join("sid2.json").exists());
    }

    #[test]
    fn no_session_id_runs_inline() {
        inline_off();
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("csr-engine.db");
        let spawner = Recording(Default::default());
        assert!(!detach_stop_with(
            &db,
            tmp.path(),
            &HookInput::default(),
            &spawner
        ));
        assert!(spawner.0.lock().unwrap().is_empty());
    }

    #[test]
    fn the_newest_spool_replaces_the_older_one() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("stop-work");
        let a = write_spool(&dir, "s", &input("s", "/t/a.jsonl")).unwrap();
        let b = write_spool(&dir, "s", &input("s", "/t/b.jsonl")).unwrap();
        assert_ne!(a, b);
        let now = read_spool(&spool_path(&dir, "s")).unwrap();
        assert_eq!(now.seq, b);
        assert_eq!(now.input.transcript_path.as_deref(), Some("/t/b.jsonl"));
    }

    #[test]
    fn a_second_worker_cannot_take_the_lock_and_leaves_the_spool() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("stop-work");
        write_spool(&dir, "s", &input("s", "/t/a.jsonl")).unwrap();
        let holder = KeyLock::try_acquire(&dir, "s").expect("first lock");
        assert!(
            KeyLock::try_acquire(&dir, "s").is_none(),
            "one worker per session"
        );
        assert!(has_pending(&dir, "s"), "the spool stays for the holder");
        drop(holder);
        assert!(KeyLock::try_acquire(&dir, "s").is_some());
    }

    #[test]
    fn a_stop_landing_during_a_run_is_run_next_and_a_burst_costs_two_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("stop-work");
        write_spool(&dir, "s", &input("s", "/t/1.jsonl")).unwrap();
        let lock = KeyLock::try_acquire(&dir, "s").unwrap();
        let mut processed = Vec::new();
        let mut first = true;
        while let Some(claimed) = lock.claim_next() {
            processed.push(claimed.transcript_path.clone().unwrap());
            if first {
                // Two more Stops arrive while the first run is in flight; the
                // third replaces the second.
                write_spool(&dir, "s", &input("s", "/t/2.jsonl")).unwrap();
                write_spool(&dir, "s", &input("s", "/t/3.jsonl")).unwrap();
                first = false;
            }
            lock.finish();
        }
        assert_eq!(processed, ["/t/1.jsonl", "/t/3.jsonl"]);
        assert!(!has_pending(&dir, "s"), "nothing left after the last run");
    }

    #[test]
    fn a_dead_workers_claim_is_run_unless_a_newer_spool_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("stop-work");
        // Died mid-run: the claim is still there, no new spool.
        write_spool(&dir, "s", &input("s", "/t/old.jsonl")).unwrap();
        std::fs::rename(spool_path(&dir, "s"), work_path(&dir, "s")).unwrap();
        let lock = KeyLock::try_acquire(&dir, "s").unwrap();
        let rerun = lock.claim_next().expect("the claim is run again");
        assert_eq!(rerun.transcript_path.as_deref(), Some("/t/old.jsonl"));
        lock.finish();
        assert!(lock.claim_next().is_none());

        // Died mid-run and a newer Stop wrote a spool: only the newer one runs.
        write_spool(&dir, "s", &input("s", "/t/old.jsonl")).unwrap();
        std::fs::rename(spool_path(&dir, "s"), work_path(&dir, "s")).unwrap();
        write_spool(&dir, "s", &input("s", "/t/new.jsonl")).unwrap();
        let next = lock.claim_next().unwrap();
        assert_eq!(next.transcript_path.as_deref(), Some("/t/new.jsonl"));
        lock.finish();
        assert!(lock.claim_next().is_none());
    }

    #[test]
    fn a_corrupt_spool_is_dropped_not_looped_on() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("stop-work");
        make_dir(&dir).unwrap();
        std::fs::write(spool_path(&dir, "s"), b"{not json").unwrap();
        let lock = KeyLock::try_acquire(&dir, "s").unwrap();
        assert!(lock.claim_next().is_none());
        assert!(!has_pending(&dir, "s"));
    }

    #[test]
    fn only_a_spool_past_a_day_with_no_holder_is_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("stop-work");
        write_spool(&dir, "old", &input("old", "/t/o.jsonl")).unwrap();
        write_spool(&dir, "fresh", &input("fresh", "/t/f.jsonl")).unwrap();
        write_spool(&dir, "self", &input("self", "/t/s.jsonl")).unwrap();
        let later = SystemTime::now() + STALE_AFTER + Duration::from_secs(60);
        let mut keys = stale_keys(&dir, "self", later);
        keys.sort();
        assert_eq!(
            keys,
            ["fresh", "old"],
            "all but the worker's own are a day old by then"
        );
        assert!(stale_keys(&dir, "self", SystemTime::now()).is_empty());
    }
}
