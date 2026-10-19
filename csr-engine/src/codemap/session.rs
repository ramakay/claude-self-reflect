//! The code map for a session that works inside a macOS-protected folder.
//!
//! The daemon never stats, watches, polls, scans or drains anything under
//! Desktop, Documents, Downloads, iCloud Drive or `/Volumes/<name>`: macOS pops a
//! privacy prompt for the signed binary that does. A user who starts Claude Code
//! in `~/Documents/proj` has still opened that folder themselves, so the SESSION
//! maps it, through two short-lived children the hooks start:
//!
//! - **SessionStart** in a protected cwd spawns `csr-engine --db-path <db> codemap
//!   scan --repo <cwd> --session`: one scan of the repo and its worktrees.
//! - **PostToolUse** in a protected cwd enqueues the edited file (the daemon leaves
//!   protected queue rows alone) and spawns `csr-engine --db-path <db> codemap
//!   drain --repo <cwd>` unless one is already in flight. The drain waits out a
//!   debounce, rescans the queued paths of that repo (or the whole repo when HEAD
//!   or the index moved), repeats while new rows arrive, then exits.
//!
//! Both children are DETACHED: null stdio, their own process group, never waited
//! on, so the hook returns at once. The `--session` opt-in is a thread-local scope
//! (`identity::session_scope`) that opens only the protected folder the session's
//! cwd is in, and only in those two processes.
//!
//! "One drain in flight" is a lock file, `<data_dir>/codemap/session-<hash>.lock`,
//! created with `create_new` by the hook and holding `pid=<child> start=<secs>`.
//! The child removes it on exit. A lock is stale after ten minutes, or when its
//! pid is gone, and is then replaced. One spawn per hook call at most, no retries.
//!
//! Lag, stated plainly: an edit made OUTSIDE Claude in a protected repo is not
//! seen until the next session event (a SessionStart or a Claude edit in that
//! cwd), because nothing watches the folder.
//!
//! An ordinary git repo is mapped by the daemon's loop. A fresh install has no
//! daemon (setup never starts one), so with NO live daemon the session maps an
//! ordinary repo the same way, through the same two children
//! (`on_session_start_ordinary`, `on_post_tool_use_ordinary`). A live daemon
//! keeps ownership: the hooks do nothing there. The scan child registers such a
//! repo with source `signal`, as the daemon would.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use rusqlite::Connection;

use super::identity::{self, is_tcc_path, normalize_lexical};
use crate::extraction::repo_scan::has_code_extension;

/// A session lock older than this is stale whatever its pid says.
pub const LOCK_STALE_SECS: i64 = 600;

/// How long a `drain` waits for further edits before it rescans (`--debounce`).
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_secs(5);

/// A `drain` ends after this long even if edits keep arriving: the next hook
/// starts a new one, and the lock goes stale at ten minutes.
pub const DRAIN_MAX: Duration = Duration::from_secs(9 * 60);

/// Starts the detached children. A trait so tests assert the argv instead of
/// spawning a process.
pub trait Spawner {
    /// Start `current_exe <argv>` detached; returns the child's pid.
    fn spawn(&self, argv: &[OsString]) -> std::io::Result<u32>;
}

/// The real spawner: null stdio, its own process group, never waited on.
pub struct DetachedSpawner;

impl Spawner for DetachedSpawner {
    // The hook exits right after; init reaps the child. Waiting would hold the
    // hook until the scan ends.
    #[allow(clippy::zombie_processes)]
    fn spawn(&self, argv: &[OsString]) -> std::io::Result<u32> {
        let mut cmd = Command::new(std::env::current_exe()?);
        cmd.args(argv)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        Ok(cmd.spawn()?.id())
    }
}

fn base_argv(db_path: &Path, sub: &str) -> Vec<OsString> {
    vec![
        "--db-path".into(),
        db_path.as_os_str().to_os_string(),
        "codemap".into(),
        sub.into(),
        "--repo".into(),
    ]
}

/// `--db-path <db> codemap scan --repo <cwd> --session`.
pub fn scan_argv(db_path: &Path, cwd: &Path) -> Vec<OsString> {
    let mut argv = base_argv(db_path, "scan");
    argv.push(cwd.as_os_str().to_os_string());
    argv.push("--session".into());
    argv
}

/// `--db-path <db> codemap drain --repo <cwd>`.
pub fn drain_argv(db_path: &Path, cwd: &Path) -> Vec<OsString> {
    let mut argv = base_argv(db_path, "drain");
    argv.push(cwd.as_os_str().to_os_string());
    argv
}

// ---- the lock file ---------------------------------------------------------------

/// `<data_dir>/codemap/session-<16 hex of blake3(cwd)>.lock`. The cwd is hashed as
/// the hook was given it (lexically normalised): the hook may not stat a protected
/// folder to resolve it, and the child is started with the same string.
pub fn lock_path(db_path: &Path, cwd: &Path) -> PathBuf {
    let data_dir = match db_path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let norm = normalize_lexical(cwd);
    let hash = blake3::hash(norm.to_string_lossy().as_bytes()).to_hex();
    data_dir
        .join("codemap")
        .join(format!("session-{}.lock", &hash.as_str()[..16]))
}

/// A taken session lock. Dropping it does NOT remove the file: the child owns the
/// file from then on and removes it when it exits (`LockHold`).
#[derive(Debug)]
pub struct SessionLock {
    path: PathBuf,
}

impl SessionLock {
    /// Record the child's pid (`pid=0` until it is known). Never recreates a file
    /// the child has already removed.
    pub(crate) fn set_pid(&self, pid: u32, now: i64) {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&self.path)
        {
            let _ = writeln!(f, "pid={pid} start={now}");
        }
    }

    /// Remove the file: the spawn failed, so no child owns it.
    fn release(self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn read_lock(path: &Path) -> Option<(u32, i64)> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut pid = None;
    let mut start = None;
    for part in text.split_whitespace() {
        if let Some(v) = part.strip_prefix("pid=") {
            pid = v.parse().ok();
        } else if let Some(v) = part.strip_prefix("start=") {
            start = v.parse().ok();
        }
    }
    Some((pid?, start?))
}

/// Is `pid` a live process? (`kill(pid, 0)`: EPERM still means it exists.)
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        // SAFETY: signal 0 only checks that the process exists.
        let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
        r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Take the session lock at `path`, or `None` when a drain is in flight. A
/// lock is in flight when it is younger than `LOCK_STALE_SECS` and its pid is
/// alive (or not recorded yet). A stale one is removed and taken over.
pub fn try_take(
    path: &Path,
    now: i64,
    alive: &dyn Fn(u32) -> bool,
) -> std::io::Result<Option<SessionLock>> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    for _ in 0..2 {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(mut f) => {
                let _ = writeln!(f, "pid=0 start={now}");
                return Ok(Some(SessionLock {
                    path: path.to_path_buf(),
                }));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let in_flight = match read_lock(path) {
                    Some((pid, start)) => {
                        now.saturating_sub(start) <= LOCK_STALE_SECS && (pid == 0 || alive(pid))
                    }
                    // Created a moment ago and not written yet, or unreadable:
                    // judge it by its age on disk.
                    None => std::fs::metadata(path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_none_or(|age| age.as_secs() as i64 <= LOCK_STALE_SECS),
                };
                if in_flight {
                    return Ok(None);
                }
                let _ = std::fs::remove_file(path);
            }
            Err(e) => return Err(e),
        }
    }
    Ok(None)
}

/// Held by the `drain` child: removes the lock file when it exits, but only if the
/// file is still this process's (or not yet attributed).
pub struct LockHold {
    path: PathBuf,
    /// False once `release_and_recheck` has handed the lock over: the file is then
    /// not this child's to touch (a hook may already have made a new one).
    armed: bool,
}

impl LockHold {
    pub fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    /// Remove the lock file, only if it is still this process's or not yet
    /// attributed to a child.
    fn remove_if_own(&self) {
        match read_lock(&self.path) {
            Some((pid, _)) if pid != 0 && pid != std::process::id() => {}
            _ => {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

impl Drop for LockHold {
    fn drop(&mut self) {
        if self.armed {
            self.remove_if_own();
        }
    }
}

/// What a drain does after a pass found nothing to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Handoff {
    /// Rows arrived and this child holds the lock again: run another pass.
    Continue,
    /// Nothing is left, or another drain owns what is: exit.
    Exit,
}

/// The exit sequence of a drain whose last pass found nothing: release the lock
/// file, THEN read the queue again (`rows_left`). If rows are there, take the same
/// lock (`try_take`), record this pid and carry on; if it is taken, another drain
/// owns the rows and this one exits.
///
/// Why no edit is stranded. A hook does two things in this order: enqueue the
/// row, then `try_take` the lock (live lock: `InFlight`, no spawn). Call our
/// release R and our queue read Q, with R before Q.
///
/// - A hook whose `try_take` came before R found our lock and spawned nothing.
///   Its enqueue came before its `try_take`, so before R, so before Q: Q sees
///   the row.
/// - A hook whose `try_take` came after R finds no live lock, takes it and
///   spawns its own drain, whatever Q saw. If Q also saw its row, our `try_take`
///   finds that lock and we exit; the row has an owner either way.
///
/// So every row is seen by Q or owned by a freshly spawned drain; the old exit
/// (read empty, then remove the file) had a gap between the read and the
/// removal in which a hook could enqueue, see the live lock, and leave.
pub fn release_and_recheck(
    hold: &mut LockHold,
    now: i64,
    alive: &dyn Fn(u32) -> bool,
    rows_left: &mut dyn FnMut() -> bool,
) -> Handoff {
    hold.remove_if_own();
    if !rows_left() {
        hold.armed = false;
        return Handoff::Exit;
    }
    match try_take(&hold.path, now, alive) {
        Ok(Some(lock)) => {
            lock.set_pid(std::process::id(), now);
            Handoff::Continue
        }
        // Another drain took it (or the lock cannot be made): not ours any more.
        _ => {
            hold.armed = false;
            Handoff::Exit
        }
    }
}

/// Test-only seam: a closure run by `cli::drain` right after a pass found
/// nothing, before it releases its lock, so a test can queue an edit in the
/// window the exit sequence has to cover. Thread-local.
#[cfg(test)]
pub(crate) mod test_hooks {
    use std::cell::RefCell;

    type Hook = Box<dyn FnMut()>;

    thread_local! {
        static AFTER_IDLE: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }

    pub(crate) fn set_after_idle(hook: impl FnMut() + 'static) {
        AFTER_IDLE.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
    }

    pub(crate) fn after_idle() {
        // Taken out first so the hook may itself call back into this module.
        let hook = AFTER_IDLE.with(|h| h.borrow_mut().take());
        if let Some(mut f) = hook {
            f();
        }
    }
}

// ---- the hook side -----------------------------------------------------------------

/// SessionStart in `cwd`: when it is in a protected folder, spawn the detached
/// session scan. Returns the child's pid. Nothing happens when the code map is
/// off, `cwd` is relative or ordinary. No filesystem call under a protected folder.
pub fn on_session_start(
    cwd: &Path,
    db_path: &Path,
    home: Option<&Path>,
    disabled: bool,
    spawner: &dyn Spawner,
) -> Option<u32> {
    if disabled || !cwd.is_absolute() {
        return None;
    }
    let cwd = normalize_lexical(cwd);
    identity::protected_class(&cwd, home)?;
    match spawner.spawn(&scan_argv(db_path, &cwd)) {
        Ok(pid) => Some(pid),
        Err(e) => {
            eprintln!("CSR: code map session scan not started (non-fatal): {e}");
            None
        }
    }
}

/// What a PostToolUse in a protected cwd came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Drained {
    /// The cwd is not in a protected folder: the daemon's own path handles it.
    NotProtected,
    /// Nothing to queue: no edit, not a code file, or not in a protected folder.
    NotQueued,
    /// Queued; a drain is already in flight and will take the row.
    InFlight,
    /// Queued; a drain was spawned (child pid).
    Spawned(u32),
    /// The queue write or the spawn failed (never fatal).
    Failed,
}

/// The real path of `edited` when it lies in the same protected folder as the
/// session's `cwd`, else `None`. The hook runs under the session's own macOS
/// grant, so a scope for `cwd` lets `resolve_safe` look inside that one folder
/// (and no other; a chain through any other protected folder is still refused).
/// The parent is resolved and the file name joined on, so a file that was just
/// deleted still resolves. The scope is dropped before this returns.
fn resolve_in_session_root(
    cwd: &Path,
    edited: &Path,
    home: Option<&Path>,
    class: &identity::TccRoot,
) -> Option<PathBuf> {
    let _scope = identity::session_scope(cwd, home);
    let real = identity::resolve_safe(edited.parent()?, home)
        .ok()?
        .join(edited.file_name()?);
    (identity::protected_class(&real, home)?.key == class.key).then_some(real)
}

/// PostToolUse in `cwd` after an edit of `edited`: enqueue the file, and spawn a
/// detached `codemap drain` unless one is in flight. One spawn per call at most,
/// no retries.
#[allow(clippy::too_many_arguments)]
pub fn on_post_tool_use(
    conn: &Connection,
    cwd: &Path,
    edited: Option<&Path>,
    db_path: &Path,
    home: Option<&Path>,
    disabled: bool,
    now: i64,
    alive: &dyn Fn(u32) -> bool,
    spawner: &dyn Spawner,
) -> Drained {
    if disabled || !cwd.is_absolute() {
        return Drained::NotProtected;
    }
    let cwd = normalize_lexical(cwd);
    let Some(class) = identity::protected_class(&cwd, home) else {
        return Drained::NotProtected;
    };
    let Some(edited) = edited.filter(|p| p.is_absolute() && has_code_extension(p)) else {
        return Drained::NotQueued;
    };
    // A path that is lexically protected is queued as it is. One that is not may
    // still be an alias of the session's folder (`/work/project` linked to
    // `~/Documents/project`): the drain matches rows against RESOLVED view roots,
    // so the alias is resolved here and the resolved path queued.
    let resolved;
    let edited = if is_tcc_path(edited, home) {
        edited
    } else {
        match resolve_in_session_root(&cwd, edited, home, &class) {
            Some(real) => {
                resolved = real;
                resolved.as_path()
            }
            None => return Drained::NotQueued,
        }
    };
    if let Err(e) = super::with_hook_busy_timeout(conn, || super::enqueue(conn, edited)) {
        eprintln!("CSR: code map session enqueue skipped (non-fatal): {e}");
        return Drained::Failed;
    }
    let lock = match try_take(&lock_path(db_path, &cwd), now, alive) {
        Ok(Some(lock)) => lock,
        Ok(None) => return Drained::InFlight,
        Err(e) => {
            eprintln!("CSR: code map session lock failed (non-fatal): {e}");
            return Drained::Failed;
        }
    };
    match spawner.spawn(&drain_argv(db_path, &cwd)) {
        Ok(pid) => {
            lock.set_pid(pid, now);
            Drained::Spawned(pid)
        }
        Err(e) => {
            eprintln!("CSR: code map session drain not started (non-fatal): {e}");
            lock.release();
            Drained::Failed
        }
    }
}

// ---- ordinary repos, no daemon -------------------------------------------------------

/// `cwd`, normalised, when the session itself should map it: absolute, NOT in a
/// protected folder (the functions above own those), inside a git repo, the code
/// map on, and no live daemon (`daemon_live`) to do it. The repo check walks up
/// from `cwd`, which is safe: `identify_with` refuses a protected path before any
/// filesystem call.
fn ordinary_repo_cwd(
    cwd: &Path,
    home: Option<&Path>,
    disabled: bool,
    daemon_live: bool,
) -> Option<PathBuf> {
    if disabled || daemon_live || !cwd.is_absolute() {
        return None;
    }
    let cwd = normalize_lexical(cwd);
    if identity::protected_class(&cwd, home).is_some() {
        return None;
    }
    identity::identify_with(&cwd, home).ok()?;
    Some(cwd)
}

/// SessionStart in an ordinary git repo while no daemon runs: spawn the detached
/// session scan, exactly as for a protected folder. Returns the child's pid.
/// Nothing when the map is off, a daemon is live, `cwd` is relative, protected
/// (`on_session_start`'s case) or not in a git repo.
pub fn on_session_start_ordinary(
    cwd: &Path,
    db_path: &Path,
    home: Option<&Path>,
    disabled: bool,
    daemon_live: bool,
    spawner: &dyn Spawner,
) -> Option<u32> {
    let cwd = ordinary_repo_cwd(cwd, home, disabled, daemon_live)?;
    match spawner.spawn(&scan_argv(db_path, &cwd)) {
        Ok(pid) => Some(pid),
        Err(e) => {
            eprintln!("CSR: code map session scan not started (non-fatal): {e}");
            None
        }
    }
}

/// PostToolUse after an edit in an ordinary git repo while no daemon runs. The
/// generic hook has already queued the edited file (the daemon would have drained
/// it), so this only starts the child: a detached `codemap drain` unless one is
/// in flight, or, for a repo the map does not know yet (the SessionStart scan
/// never ran), a detached `codemap scan --session` that registers it. One spawn
/// per call at most, no retries.
#[allow(clippy::too_many_arguments)]
pub fn on_post_tool_use_ordinary(
    conn: &Connection,
    cwd: &Path,
    edited: Option<&Path>,
    db_path: &Path,
    home: Option<&Path>,
    disabled: bool,
    daemon_live: bool,
    now: i64,
    alive: &dyn Fn(u32) -> bool,
    spawner: &dyn Spawner,
) -> Drained {
    // The cheap gate first: most PostToolUse calls are not code edits.
    if !edited.is_some_and(|p| p.is_absolute() && has_code_extension(p)) {
        return Drained::NotQueued;
    }
    let Some(cwd) = ordinary_repo_cwd(cwd, home, disabled, daemon_live) else {
        return Drained::NotProtected;
    };
    let Ok(id) = identity::identify_with(&cwd, home) else {
        return Drained::NotQueued;
    };
    let common = id.common_dir.to_string_lossy().to_string();
    let known = super::store::list_repos(conn)
        .map(|repos| repos.iter().any(|r| r.common_dir == common))
        .unwrap_or(false);
    if !known {
        return match spawner.spawn(&scan_argv(db_path, &cwd)) {
            Ok(pid) => Drained::Spawned(pid),
            Err(e) => {
                eprintln!("CSR: code map session scan not started (non-fatal): {e}");
                Drained::Failed
            }
        };
    }
    let lock = match try_take(&lock_path(db_path, &cwd), now, alive) {
        Ok(Some(lock)) => lock,
        Ok(None) => return Drained::InFlight,
        Err(e) => {
            eprintln!("CSR: code map session lock failed (non-fatal): {e}");
            return Drained::Failed;
        }
    };
    match spawner.spawn(&drain_argv(db_path, &cwd)) {
        Ok(pid) => {
            lock.set_pid(pid, now);
            Drained::Spawned(pid)
        }
        Err(e) => {
            eprintln!("CSR: code map session drain not started (non-fatal): {e}");
            lock.release();
            Drained::Failed
        }
    }
}

// ---- the child side ----------------------------------------------------------------

/// Queue rows (`path`, stamp) that lie under one of the repo's view roots: the
/// rows THIS repo's drain owns.
pub fn rows_under(conn: &Connection, roots: &[PathBuf]) -> anyhow::Result<Vec<(String, i64)>> {
    Ok(super::store::queue_read(conn)?
        .into_iter()
        .filter(|(p, _)| roots.iter().any(|r| Path::new(p).starts_with(r)))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codemap::store;
    use crate::codemap::testutil::db;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Recorder {
        calls: RefCell<Vec<Vec<OsString>>>,
        next_pid: RefCell<u32>,
        fail: bool,
    }

    impl Recorder {
        fn new() -> Self {
            Self {
                next_pid: RefCell::new(4242),
                ..Default::default()
            }
        }
        fn args(&self) -> Vec<Vec<String>> {
            self.calls
                .borrow()
                .iter()
                .map(|c| c.iter().map(|a| a.to_string_lossy().to_string()).collect())
                .collect()
        }
    }

    impl Spawner for Recorder {
        fn spawn(&self, argv: &[OsString]) -> std::io::Result<u32> {
            if self.fail {
                return Err(std::io::Error::other("no exe"));
            }
            self.calls.borrow_mut().push(argv.to_vec());
            let pid = *self.next_pid.borrow();
            *self.next_pid.borrow_mut() += 1;
            Ok(pid)
        }
    }

    /// A fake home with fake Desktop/Documents/Downloads, and a data dir.
    struct Fake {
        _tmp: tempfile::TempDir,
        home: PathBuf,
        db: PathBuf,
    }

    fn fake() -> Fake {
        let tmp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(tmp.path()).unwrap();
        let home = base.join("home");
        for d in ["Desktop", "Documents", "Downloads"] {
            std::fs::create_dir_all(home.join(d)).unwrap();
        }
        std::fs::create_dir_all(base.join("data")).unwrap();
        Fake {
            db: base.join("data/csr-engine.db"),
            home,
            _tmp: tmp,
        }
    }

    #[test]
    fn session_start_in_a_protected_cwd_spawns_one_scan_with_the_expected_argv() {
        let f = fake();
        let rec = Recorder::new();
        let cwd = f.home.join("Documents/probe/src");
        let pid = on_session_start(&cwd, &f.db, Some(&f.home), false, &rec);
        assert_eq!(pid, Some(4242));
        let want = vec![
            "--db-path".to_string(),
            f.db.to_string_lossy().to_string(),
            "codemap".into(),
            "scan".into(),
            "--repo".into(),
            cwd.to_string_lossy().to_string(),
            "--session".into(),
        ];
        assert_eq!(rec.args(), vec![want], "exactly one spawn, this argv");
    }

    #[test]
    fn session_start_spawns_for_a_symlink_chain_into_a_protected_folder() {
        let f = fake();
        std::fs::create_dir_all(f.home.join("Desktop/p")).unwrap();
        let hop = f.home.join("hop");
        std::os::unix::fs::symlink(f.home.join("Desktop/p"), &hop).unwrap();
        let rec = Recorder::new();
        assert!(on_session_start(&hop, &f.db, Some(&f.home), false, &rec).is_some());
        assert_eq!(rec.args().len(), 1);
    }

    #[test]
    fn session_start_spawns_nothing_for_ordinary_relative_or_disabled_cwds() {
        let f = fake();
        let rec = Recorder::new();
        let plain = f.home.join("projects/x");
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(
            on_session_start(&plain, &f.db, Some(&f.home), false, &rec),
            None
        );
        assert_eq!(
            on_session_start(Path::new("rel/dir"), &f.db, Some(&f.home), false, &rec),
            None
        );
        let docs = f.home.join("Documents/p");
        assert_eq!(
            on_session_start(&docs, &f.db, Some(&f.home), true, &rec),
            None
        );
        assert!(rec.args().is_empty());
        // A failing spawn is swallowed, not an error.
        let failing = Recorder {
            fail: true,
            ..Recorder::new()
        };
        assert_eq!(
            on_session_start(&docs, &f.db, Some(&f.home), false, &failing),
            None
        );
    }

    #[test]
    fn the_hook_side_makes_no_filesystem_call_under_a_protected_folder() {
        use std::os::unix::fs::PermissionsExt;
        let f = fake();
        let docs = f.home.join("Documents");
        // Mode 000: a stat of anything below it fails with EACCES, which would
        // change what a resolver reports. The lexical rule answers first.
        std::fs::set_permissions(&docs, std::fs::Permissions::from_mode(0o000)).unwrap();
        let rec = Recorder::new();
        let cwd = docs.join("probe");
        let pid = on_session_start(&cwd, &f.db, Some(&f.home), false, &rec);
        let conn = db();
        let out = on_post_tool_use(
            &conn,
            &cwd,
            Some(&cwd.join("a.rs")),
            &f.db,
            Some(&f.home),
            false,
            1_000,
            &|_| false,
            &rec,
        );
        std::fs::set_permissions(&docs, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(pid.is_some());
        assert!(matches!(out, Drained::Spawned(_)), "{out:?}");
    }

    fn edit(
        f: &Fake,
        conn: &Connection,
        rec: &Recorder,
        now: i64,
        alive: &dyn Fn(u32) -> bool,
    ) -> Drained {
        let cwd = f.home.join("Documents/probe");
        on_post_tool_use(
            conn,
            &cwd,
            Some(&cwd.join("src/a.rs")),
            &f.db,
            Some(&f.home),
            false,
            now,
            alive,
            rec,
        )
    }

    #[test]
    fn post_tool_use_spawns_one_drain_while_the_lock_is_held_by_a_live_pid_and_again_after_it_goes_stale(
    ) {
        let f = fake();
        let conn = db();
        let rec = Recorder::new();
        let alive = |_: u32| true;

        assert_eq!(edit(&f, &conn, &rec, 1_000, &alive), Drained::Spawned(4242));
        let cwd = f.home.join("Documents/probe");
        let want = vec![
            "--db-path".to_string(),
            f.db.to_string_lossy().to_string(),
            "codemap".into(),
            "drain".into(),
            "--repo".into(),
            cwd.to_string_lossy().to_string(),
        ];
        assert_eq!(rec.args(), vec![want.clone()]);
        // The lock names the child.
        let lock = lock_path(&f.db, &cwd);
        assert_eq!(read_lock(&lock), Some((4242, 1_000)));

        // More edits while that pid lives: rows queue, nothing spawns.
        for t in [1_001, 1_100, 1_599] {
            assert_eq!(edit(&f, &conn, &rec, t, &alive), Drained::InFlight);
        }
        assert_eq!(rec.args().len(), 1, "still one spawn");
        assert_eq!(
            store::queue_read(&conn).unwrap().len(),
            1,
            "the same file is one row"
        );

        // The pid is gone: the lock is replaced and a drain spawns again.
        let dead = |_: u32| false;
        assert_eq!(edit(&f, &conn, &rec, 1_200, &dead), Drained::Spawned(4243));
        assert_eq!(rec.args().len(), 2);
        assert_eq!(read_lock(&lock), Some((4243, 1_200)));

        // Ten minutes later a live-looking pid no longer counts either.
        assert_eq!(
            edit(&f, &conn, &rec, 1_200 + LOCK_STALE_SECS + 1, &alive),
            Drained::Spawned(4244)
        );
        assert_eq!(rec.args()[2], want);
    }

    #[test]
    fn a_failed_spawn_releases_the_lock_so_the_next_edit_can_try_and_nothing_retries() {
        let f = fake();
        let conn = db();
        let failing = Recorder {
            fail: true,
            ..Recorder::new()
        };
        assert_eq!(edit(&f, &conn, &failing, 5, &|_| true), Drained::Failed);
        assert!(!lock_path(&f.db, &f.home.join("Documents/probe")).exists());
        let ok = Recorder::new();
        assert_eq!(edit(&f, &conn, &ok, 6, &|_| true), Drained::Spawned(4242));
    }

    #[test]
    fn post_tool_use_queues_only_protected_code_files_of_a_protected_cwd() {
        let f = fake();
        let conn = db();
        let rec = Recorder::new();
        let cwd = f.home.join("Documents/probe");
        let run = |cwd: &Path, edited: Option<&Path>, disabled: bool| {
            on_post_tool_use(
                &conn,
                cwd,
                edited,
                &f.db,
                Some(&f.home),
                disabled,
                10,
                &|_| true,
                &rec,
            )
        };
        assert_eq!(run(&cwd, None, false), Drained::NotQueued, "no edit");
        assert_eq!(
            run(&cwd, Some(&cwd.join("README.md")), false),
            Drained::NotQueued,
            "not a code file"
        );
        assert_eq!(
            run(&cwd, Some(Path::new("/tmp/x.rs")), false),
            Drained::NotQueued,
            "an ordinary path is the daemon's"
        );
        assert_eq!(
            run(&cwd, Some(Path::new("rel/a.rs")), false),
            Drained::NotQueued
        );
        let plain = f.home.join("projects/x");
        assert_eq!(
            run(&plain, Some(&cwd.join("a.rs")), false),
            Drained::NotProtected,
            "an ordinary cwd is not a session cwd"
        );
        assert_eq!(
            run(&cwd, Some(&cwd.join("a.rs")), true),
            Drained::NotProtected,
            "CSR_NO_CODEMAP"
        );
        assert!(store::queue_read(&conn).unwrap().is_empty());
        assert!(rec.args().is_empty());
        assert_eq!(
            run(&cwd, Some(&cwd.join("a.rs")), false),
            Drained::Spawned(4242)
        );
        assert_eq!(store::queue_read(&conn).unwrap().len(), 1);
    }

    #[test]
    fn the_lock_is_replaced_when_unreadable_and_old_and_kept_when_unreadable_and_new() {
        let f = fake();
        let path = lock_path(&f.db, &f.home.join("Documents/probe"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "").unwrap();
        // Empty and just created (another hook is between create and write).
        assert!(try_take(&path, 100, &|_| false).unwrap().is_none());
        // Garbage that is old on disk.
        std::fs::write(&path, "garbage").unwrap();
        let old = std::time::SystemTime::now() - Duration::from_secs(3_600);
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_modified(old).unwrap();
        drop(file);
        assert!(try_take(&path, 100, &|_| true).unwrap().is_some());
    }

    #[test]
    fn the_child_removes_its_own_lock_on_exit_and_leaves_another_childs() {
        let f = fake();
        let path = lock_path(&f.db, &f.home.join("Documents/probe"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("pid={} start=1\n", std::process::id())).unwrap();
        drop(LockHold::new(path.clone()));
        assert!(!path.exists(), "its own lock goes");
        std::fs::write(&path, "pid=999999 start=1\n").unwrap();
        drop(LockHold::new(path.clone()));
        assert!(path.exists(), "another child's lock stays");
        std::fs::write(&path, "pid=0 start=1\n").unwrap();
        drop(LockHold::new(path.clone()));
        assert!(!path.exists(), "an unattributed lock goes");
    }

    #[test]
    fn rows_under_picks_only_the_rows_of_the_repo_roots() {
        let conn = db();
        for p in [
            "/h/Documents/a/x.rs",
            "/h/Documents/b/y.rs",
            "/h/Documents/a2/z.rs",
        ] {
            crate::codemap::enqueue(&conn, Path::new(p)).unwrap();
        }
        let rows = rows_under(&conn, &[PathBuf::from("/h/Documents/a")]).unwrap();
        let paths: Vec<&str> = rows.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, ["/h/Documents/a/x.rs"], "a2 is not under a");
    }

    #[test]
    fn the_lock_path_is_stable_per_cwd_and_lives_next_to_the_database() {
        let db = Path::new("/d/csr-engine.db");
        let a = lock_path(db, Path::new("/h/Documents/p"));
        assert_eq!(a, lock_path(db, Path::new("/h/Documents/./p/")));
        assert_ne!(a, lock_path(db, Path::new("/h/Documents/q")));
        assert_eq!(a.parent(), Some(Path::new("/d/codemap")));
        let name = a.file_name().unwrap().to_string_lossy().to_string();
        assert!(
            name.starts_with("session-") && name.ends_with(".lock"),
            "{name}"
        );
    }

    // ---- exit handoff (finding 7) --------------------------------------------------

    fn held_lock(f: &Fake) -> (PathBuf, LockHold) {
        let path = lock_path(&f.db, &f.home.join("Documents/probe"));
        let lock = try_take(&path, 100, &|_| false).unwrap().unwrap();
        lock.set_pid(std::process::id(), 100);
        (path.clone(), LockHold::new(path))
    }

    #[test]
    fn handoff_with_nothing_queued_releases_the_lock_before_it_reads_the_queue() {
        let f = fake();
        let (path, mut hold) = held_lock(&f);
        let mut seen_released = false;
        let out = release_and_recheck(&mut hold, 200, &|_| true, &mut || {
            seen_released = !path.exists();
            false
        });
        assert_eq!(out, Handoff::Exit);
        assert!(seen_released, "the lock was released before the queue read");
        drop(hold);
        assert!(!path.exists());
    }

    #[test]
    fn handoff_with_a_row_queued_in_the_window_takes_the_lock_again_and_continues() {
        let f = fake();
        let (path, mut hold) = held_lock(&f);
        let out = release_and_recheck(&mut hold, 200, &|_| true, &mut || true);
        assert_eq!(out, Handoff::Continue);
        let (pid, _) = read_lock(&path).expect("the lock is held again");
        assert_eq!(pid, std::process::id());
        // Still this child's: dropping the hold removes it on the way out.
        drop(hold);
        assert!(!path.exists());
    }

    #[test]
    fn handoff_leaves_a_lock_a_new_hook_took_after_the_release() {
        let f = fake();
        let (path, mut hold) = held_lock(&f);
        let p = path.clone();
        let out = release_and_recheck(&mut hold, 200, &|_| true, &mut || {
            // A hook arrives between our release and our queue read: it sees no
            // lock, takes it, and has spawned its own drain (pid 999999).
            std::fs::write(&p, "pid=999999 start=200\n").unwrap();
            true
        });
        assert_eq!(out, Handoff::Exit, "that drain owns the rows");
        drop(hold);
        assert!(path.exists(), "the new drain's lock is not removed by us");
        assert_eq!(read_lock(&path).unwrap().0, 999_999);
    }

    // ---- a symlinked protected cwd (finding 8) -------------------------------------

    #[test]
    fn an_edit_through_an_alias_of_the_session_folder_is_queued_by_its_real_path() {
        let f = fake();
        let conn = db();
        let rec = Recorder::new();
        let real = f.home.join("Documents/proj");
        std::fs::create_dir_all(real.join("src")).unwrap();
        let alias = f.home.join("work");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        let out = on_post_tool_use(
            &conn,
            &alias,
            Some(&alias.join("src/a.rs")),
            &f.db,
            Some(&f.home),
            false,
            10,
            &|_| true,
            &rec,
        );
        assert!(matches!(out, Drained::Spawned(_)), "{out:?}");
        let queued = store::queue_read(&conn).unwrap();
        let paths: Vec<&str> = queued.iter().map(|(p, _)| p.as_str()).collect();
        let want = real.join("src/a.rs").to_string_lossy().to_string();
        assert_eq!(
            paths,
            [want.as_str()],
            "queued by the real path, which is what the drain's view roots match"
        );
        assert_eq!(rows_under(&conn, &[real]).unwrap().len(), 1);
    }

    #[test]
    fn an_alias_edit_outside_the_session_folder_is_not_queued() {
        let f = fake();
        let conn = db();
        let rec = Recorder::new();
        let real = f.home.join("Documents/proj");
        std::fs::create_dir_all(&real).unwrap();
        let alias = f.home.join("work");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        // An ordinary folder, and a link into ANOTHER protected folder.
        let plain = f.home.join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let other = f.home.join("desk");
        std::os::unix::fs::symlink(f.home.join("Desktop"), &other).unwrap();
        for edited in [plain.join("a.rs"), other.join("a.rs")] {
            let out = on_post_tool_use(
                &conn,
                &alias,
                Some(&edited),
                &f.db,
                Some(&f.home),
                false,
                10,
                &|_| true,
                &rec,
            );
            assert_eq!(out, Drained::NotQueued, "{}", edited.display());
        }
        assert!(store::queue_read(&conn).unwrap().is_empty());
        assert!(rec.args().is_empty());
    }

    // ---- ordinary repos, no daemon ----

    /// An ordinary git repo (a bare `.git` directory is enough for `identify`).
    fn ordinary_repo(f: &Fake) -> PathBuf {
        let repo = f.db.parent().unwrap().join("work/proj");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        repo
    }

    #[test]
    fn ordinary_session_start_spawns_the_scan_argv_when_no_daemon_is_live() {
        let f = fake();
        let repo = ordinary_repo(&f);
        let rec = Recorder::new();
        let cwd = repo.join("src");
        let pid = on_session_start_ordinary(&cwd, &f.db, Some(&f.home), false, false, &rec);
        assert_eq!(pid, Some(4242));
        let want = vec![
            "--db-path".to_string(),
            f.db.to_string_lossy().to_string(),
            "codemap".into(),
            "scan".into(),
            "--repo".into(),
            cwd.to_string_lossy().to_string(),
            "--session".into(),
        ];
        assert_eq!(
            rec.args(),
            vec![want],
            "the same argv a protected folder gets"
        );
    }

    #[test]
    fn ordinary_session_start_leaves_a_live_daemon_in_charge() {
        let f = fake();
        let repo = ordinary_repo(&f);
        let rec = Recorder::new();
        assert_eq!(
            on_session_start_ordinary(&repo, &f.db, Some(&f.home), false, true, &rec),
            None
        );
        assert!(rec.args().is_empty());
    }

    #[test]
    fn ordinary_session_start_skips_off_relative_non_git_and_protected_cwds() {
        let f = fake();
        let repo = ordinary_repo(&f);
        let rec = Recorder::new();
        let plain = f.db.parent().unwrap().join("not-a-repo");
        std::fs::create_dir_all(&plain).unwrap();
        let protected = f.home.join("Documents/probe");
        std::fs::create_dir_all(protected.join(".git")).unwrap();
        // the home directory itself is never mapped
        for (cwd, disabled) in [
            (repo.clone(), true),
            (PathBuf::from("proj/src"), false),
            (plain, false),
            (protected, false),
            (f.home.clone(), false),
        ] {
            let out = on_session_start_ordinary(&cwd, &f.db, Some(&f.home), disabled, false, &rec);
            assert_eq!(out, None, "{}", cwd.display());
        }
        assert!(rec.args().is_empty());
    }

    fn ordinary_edit(
        f: &Fake,
        conn: &Connection,
        rec: &Recorder,
        file: &str,
        daemon_live: bool,
        now: i64,
    ) -> Drained {
        let repo = ordinary_repo(f);
        on_post_tool_use_ordinary(
            conn,
            &repo,
            Some(&repo.join(file)),
            &f.db,
            Some(&f.home),
            false,
            daemon_live,
            now,
            &|_| true,
            rec,
        )
    }

    #[test]
    fn ordinary_post_tool_use_scans_an_unknown_repo_then_drains_a_known_one_once() {
        let f = fake();
        let repo = ordinary_repo(&f);
        let conn = db();
        let rec = Recorder::new();

        // not registered yet: the first edit starts the session scan
        let out = ordinary_edit(&f, &conn, &rec, "src/a.rs", false, 1_000);
        assert_eq!(out, Drained::Spawned(4242));
        assert_eq!(rec.args()[0][3], "scan");

        // registered: the edit starts one drain, a second finds it in flight
        let common = repo.join(".git");
        store::upsert_repo(&conn, &common.to_string_lossy(), None, "signal", false).unwrap();
        let out = ordinary_edit(&f, &conn, &rec, "src/a.rs", false, 1_001);
        assert_eq!(out, Drained::Spawned(4243));
        assert_eq!(rec.args()[1][3], "drain");
        assert_eq!(
            ordinary_edit(&f, &conn, &rec, "src/b.rs", false, 1_002),
            Drained::InFlight
        );
        assert_eq!(rec.args().len(), 2, "no third spawn");
    }

    #[test]
    fn ordinary_post_tool_use_ignores_non_code_edits_and_a_live_daemon() {
        let f = fake();
        ordinary_repo(&f);
        let conn = db();
        let rec = Recorder::new();
        assert_eq!(
            ordinary_edit(&f, &conn, &rec, "README.txt", false, 5),
            Drained::NotQueued
        );
        assert_eq!(
            ordinary_edit(&f, &conn, &rec, "src/a.rs", true, 5),
            Drained::NotProtected
        );
        assert!(rec.args().is_empty());
    }
}
