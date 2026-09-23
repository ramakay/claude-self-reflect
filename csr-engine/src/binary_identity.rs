//! Detect when the `csr-engine` binary on disk has been replaced under a
//! still-running process, and track every live process against a shared
//! run registry so `status` can report which ones are still serving from a
//! superseded image.
//!
//! Supersedes the old `binary_stamp` module: that module recorded only the
//! *most recently started* MCP server (a single last-writer-wins file), so
//! with more than one live server it could only ever describe the newest —
//! the other N-1 stayed invisible. This module writes one record per
//! process (`run/<pid>.json`) and reports every one whose recorded identity
//! differs from what is on disk now (gap-G2 review finding 5).
//!
//! # Identity
//!
//! [`FileIdentity`] is deliberately not inode-only. On Linux ext4/tmpfs a
//! `rm` immediately followed by `cp` to the same path can reuse the just
//! freed inode number, so a genuine replacement could compare equal on
//! `dev`+`ino` alone (gap-G2 review finding 3). Composing in length +
//! mtime + ctime catches that case, and also catches an in-place overwrite
//! that keeps the same inode (some third-party install tooling copies onto
//! an existing file instead of renaming a new one over it) — the shipped
//! installers use an atomic rename precisely so this crate never has to
//! rely on ctime/mtime alone to detect *its own* upgrades, but this module
//! has no way to know a caller followed that discipline, so it does not
//! assume ino stability either way.
//!
//! # Confirming a replacement before acting
//!
//! A poll that lands while an installer is mid-write (even an atomic
//! rename's temp file, briefly, or a third-party tool overwriting in
//! place) must never be read as "replaced" and trigger the daemon's exit
//! path — that would hand launchd a truncated or non-executable image to
//! `exec` (gap-G2 review finding: daemon exit vs installer copy window).
//! [`poll_replacement`] requires the SAME new identity on two consecutive
//! polls, and requires the file to be executable with nonzero length,
//! before reporting a confirmed replacement.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};

// ─── Identity ───

/// Identity of an on-disk file, cheap to compute (a single `stat`) and
/// sensitive both to a fresh inode (an atomic rename over the old path)
/// and to an in-place rewrite of the same inode. See the module docs for
/// why length/mtime/ctime are composed in rather than relying on the
/// platform's file-index alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileIdentity {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    len: u64,
    mtime_ns: i64,
    #[cfg(unix)]
    ctime_ns: i64,
    /// Windows has no inode; `creation_time()` (100ns ticks since the FILETIME
    /// epoch) is the closest stable per-file stand-in, following the same
    /// precedent as `import::registry::file_identity` (#271) rather than
    /// pulling in a `windows-sys` dependency for the still-unstable
    /// `file_index()` (rust-lang/rust#63010).
    #[cfg(windows)]
    creation_time: u64,
}

impl FileIdentity {
    /// Capture the identity of the file at `path`. `None` when the path is
    /// unreadable (missing, permission denied, or a transient race with an
    /// in-progress replace) — callers must treat that as "unknown", never
    /// as "replaced". See [`exe_replaced`]'s fail-closed contract.
    pub fn capture(path: &Path) -> Option<Self> {
        let md = std::fs::metadata(path).ok()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Some(Self {
                dev: md.dev(),
                ino: md.ino(),
                len: md.len(),
                mtime_ns: md
                    .mtime()
                    .saturating_mul(1_000_000_000)
                    .saturating_add(md.mtime_nsec()),
                ctime_ns: md
                    .ctime()
                    .saturating_mul(1_000_000_000)
                    .saturating_add(md.ctime_nsec()),
            })
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            Some(Self {
                len: md.len(),
                creation_time: md.creation_time(),
            })
        }
    }
}

/// True when the file now at `path` has a different identity than `start`.
/// Fails closed (`false`) when the current identity cannot be read — a
/// transient stat failure must never be reported as a confirmed
/// replacement. Use [`poll_replacement`] instead of this function directly
/// wherever the answer drives a consequential action (an exit, a
/// persistence toggle): a single `exe_replaced` reading is enough to
/// surface a notice, but not enough to act on — see the module docs.
pub fn exe_replaced(start: &FileIdentity, path: &Path) -> bool {
    match FileIdentity::capture(path) {
        Some(now) => &now != start,
        None => false,
    }
}

/// Identity of the executable currently backing this process.
pub fn current_exe_identity() -> Option<FileIdentity> {
    let path = std::env::current_exe().ok()?;
    FileIdentity::capture(&path)
}

fn is_valid_executable(path: &Path) -> bool {
    let Ok(md) = std::fs::metadata(path) else {
        return false;
    };
    if md.len() == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if md.permissions().mode() & 0o111 == 0 {
            return false;
        }
    }
    true
}

/// One poll step of a replacement watch. `previous` is the identity
/// observed on the prior poll (`None` on the first poll, or whenever the
/// prior poll did not see a stable candidate). Returns
/// `(confirmed, new_previous)`.
///
/// `confirmed` is true only when the SAME new identity was observed on two
/// consecutive polls AND the file is currently a valid, non-empty,
/// executable image — see the module docs for why a single poll is not
/// enough to act on.
pub fn poll_replacement(
    start: &FileIdentity,
    previous: Option<FileIdentity>,
    path: &Path,
) -> (bool, Option<FileIdentity>) {
    let Some(now) = FileIdentity::capture(path) else {
        return (false, None);
    };
    if &now == start {
        return (false, None);
    }
    if !is_valid_executable(path) {
        return (false, None);
    }
    if previous == Some(now) {
        (true, Some(now))
    } else {
        (false, Some(now))
    }
}

/// Exit code the daemon uses to signal "binary replaced, restart me" to
/// launchd (`KeepAlive.SuccessfulExit=false` restarts on any non-zero exit).
pub const DAEMON_REPLACED_EXIT_CODE: i32 = 75;

/// Resolves once [`poll_replacement`] confirms `path` has been replaced.
/// `poll_interval` is the normal cadence between checks; once a poll first
/// observes a candidate replacement (unconfirmed), the next check runs
/// after the shorter `confirm_interval` instead of waiting a full
/// `poll_interval` again, so confirmation lands quickly after the first
/// sighting without raising the steady-state polling rate.
pub async fn wait_for_binary_replacement(
    start: FileIdentity,
    path: PathBuf,
    poll_interval: Duration,
    confirm_interval: Duration,
) {
    let mut previous: Option<FileIdentity> = None;
    loop {
        let wait = if previous.is_some() {
            confirm_interval
        } else {
            poll_interval
        };
        tokio::time::sleep(wait).await;
        let (confirmed, observed) = poll_replacement(&start, previous, &path);
        if confirmed {
            return;
        }
        previous = observed;
    }
}

// ─── MCP tool-result notice ───

/// Text appended to a tool result when this server is running a superseded
/// build. `<notice>`-wrapped so it reads as an aside, not part of the
/// tool's own answer.
pub fn replacement_notice(mtime: &str) -> String {
    format!(
        "<notice>csr-engine was upgraded on disk at {mtime}; this server is \
         running the previous build. Restart the session or reconnect the \
         MCP server.</notice>"
    )
}

fn format_mtime(path: &Path) -> Option<String> {
    let md = std::fs::metadata(path).ok()?;
    let modified = md.modified().ok()?;
    let datetime: chrono::DateTime<chrono::Utc> = modified.into();
    Some(datetime.to_rfc3339())
}

/// Shared, cheap-to-poll state for one MCP server process: caches whether
/// the on-disk binary has diverged from the one this process started from,
/// re-stat'ing at most once per `interval` regardless of how often a tool
/// call asks (spec: "check at most every 30s on a tool call").
pub struct ReplacementWatch {
    start: Option<FileIdentity>,
    exe_path: Option<PathBuf>,
    last_checked_secs: AtomicI64,
    replaced: AtomicBool,
    replaced_mtime: Mutex<Option<String>>,
}

impl ReplacementWatch {
    /// `start` should be captured as early as possible in process startup
    /// (see [`current_exe_identity`]'s doc for why: `Engine::new()` alone
    /// can take several seconds — codesign verification on first launch of
    /// a freshly installed binary, `HnswIo::load_hnsw` on a large corpus —
    /// and capturing identity only after that work finishes widens the
    /// window in which a replacement landing during startup goes
    /// permanently invisible to this process, because it would capture the
    /// NEW file's identity as its own "start" baseline).
    pub fn new(start: Option<FileIdentity>) -> Self {
        Self {
            start,
            exe_path: std::env::current_exe().ok(),
            last_checked_secs: AtomicI64::new(i64::MIN),
            replaced: AtomicBool::new(false),
            replaced_mtime: Mutex::new(None),
        }
    }

    /// Re-stats the on-disk binary at most once per `interval`. Returns the
    /// notice text to append to a tool result once (and for the rest of
    /// this process's life, since a build cannot become un-replaced) the
    /// binary has been replaced; `None` otherwise.
    pub fn check_notice(&self, interval: Duration) -> Option<String> {
        let (Some(start), Some(path)) = (self.start.as_ref(), self.exe_path.as_deref()) else {
            return None;
        };
        if !self.replaced.load(Ordering::Relaxed) {
            let now = now_secs();
            let last = self.last_checked_secs.load(Ordering::Relaxed);
            let due = last == i64::MIN || now.saturating_sub(last) >= interval.as_secs() as i64;
            if due {
                // A lost race on this store just means two threads both
                // re-stat this tick — harmless, cheap syscall, no
                // correctness impact.
                self.last_checked_secs.store(now, Ordering::Relaxed);
                if exe_replaced(start, path) {
                    if let Some(mtime) = format_mtime(path) {
                        *self.replaced_mtime.lock().unwrap() = Some(mtime);
                    }
                    self.replaced.store(true, Ordering::Relaxed);
                }
            }
        }
        self.cached_notice()
    }

    fn cached_notice(&self) -> Option<String> {
        if !self.replaced.load(Ordering::Relaxed) {
            return None;
        }
        let mtime = self
            .replaced_mtime
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| "an unknown time".to_string());
        Some(replacement_notice(&mtime))
    }

    /// True once a replacement has been confirmed by any prior
    /// `check_notice` call. Used to gate index persistence: writes belong
    /// to the current image, not this one (see [`crate::engine::Engine`]'s
    /// `skip_index_persistence`).
    pub fn is_replaced(&self) -> bool {
        self.replaced.load(Ordering::Relaxed)
    }
}

// Deliberately no `Default` impl: `new` takes `start` explicitly so every
// call site has to say where its identity baseline came from, rather than
// silently re-capturing it (late) via `current_exe_identity()`.

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Append `notice` (if any) as an extra content block on a completed tool
/// result. Pure and side-effect free so it is directly unit-testable
/// without an MCP server.
pub fn append_binary_replaced_notice(
    mut result: rmcp::model::CallToolResult,
    notice: Option<String>,
) -> rmcp::model::CallToolResult {
    if let Some(text) = notice {
        result.content.push(rmcp::model::ContentBlock::text(text));
    }
    result
}

// ─── Run registry ───

/// One process's self-reported identity, written to
/// `~/.claude-self-reflect/run/<pid>.json` at start and removed on clean
/// exit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub pid: u32,
    pub version: String,
    pub git_sha: String,
    pub exe_identity: FileIdentity,
    /// "daemon" or "mcp_stdio".
    pub role: String,
    pub started_at: String,
    pub parent_cmd: Option<String>,
}

/// `~/.claude-self-reflect/run`.
pub fn default_run_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude-self-reflect").join("run"))
}

#[cfg(unix)]
fn parent_command_name() -> Option<String> {
    // SAFETY: getppid() takes no arguments and cannot fail.
    let ppid = unsafe { libc::getppid() };
    let output = std::process::Command::new("ps")
        .args(["-o", "comm=", "-p", &ppid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

#[cfg(not(unix))]
fn parent_command_name() -> Option<String> {
    None
}

/// Best-effort: write this process's record to `<run_dir>/<pid>.json`.
/// Never fails the caller — a missing or unwritable state dir must not
/// affect serving, the same fail-open contract as the rest of this
/// crate's state files. Returns the path written (pass to
/// [`remove_run_record`] on clean exit), or `None` if the write could not
/// happen.
pub fn write_run_record(run_dir: &Path, role: &str, identity: FileIdentity) -> Option<PathBuf> {
    let pid = std::process::id();
    let record = RunRecord {
        pid,
        version: env!("CARGO_PKG_VERSION").to_string(),
        git_sha: env!("CSR_BUILD_GIT_SHA").to_string(),
        exe_identity: identity,
        role: role.to_string(),
        started_at: chrono::Utc::now().to_rfc3339(),
        parent_cmd: parent_command_name(),
    };
    std::fs::create_dir_all(run_dir).ok()?;
    let path = run_dir.join(format!("{pid}.json"));
    let json = serde_json::to_string_pretty(&record).ok()?;
    std::fs::write(&path, json).ok()?;
    Some(path)
}

/// Best-effort removal of a run record written by [`write_run_record`].
pub fn remove_run_record(path: &Path) {
    let _ = std::fs::remove_file(path);
}

#[cfg(unix)]
fn pid_is_alive(pid: u32) -> bool {
    // SAFETY: signal 0 sends no signal; it only probes existence and
    // permission. `pid` came from a JSON record we wrote ourselves.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(unix))]
fn pid_is_alive(_pid: u32) -> bool {
    // No portable liveness probe wired up on this target yet; assume alive
    // rather than risk deleting a live record (removal is safe to skip,
    // unlike a false stale-process report).
    true
}

/// One process reported as running a superseded binary.
#[derive(Debug, Clone, Serialize)]
pub struct StaleProcess {
    pub pid: u32,
    pub role: String,
    pub started_at: String,
    pub parent: Option<String>,
}

/// Read every `<run_dir>/<pid>.json` record, drop (and delete the file
/// for) any whose pid is no longer alive, and report the survivors whose
/// recorded exe identity differs from `current`.
pub fn stale_processes(run_dir: &Path, current: &FileIdentity) -> Vec<StaleProcess> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(run_dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(record) = serde_json::from_slice::<RunRecord>(&bytes) else {
            continue;
        };
        if !pid_is_alive(record.pid) {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        if &record.exe_identity != current {
            out.push(StaleProcess {
                pid: record.pid,
                role: record.role,
                started_at: record.started_at,
                parent: record.parent_cmd,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── FileIdentity / exe_replaced (spec test 3(i)) ──

    #[cfg(unix)]
    fn set_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    #[cfg(not(unix))]
    fn set_executable(_path: &Path) {}

    #[test]
    fn exe_replaced_true_after_a_real_replacement() {
        let tmp = tempfile::TempDir::new().unwrap();
        let exe = tmp.path().join("csr-engine");
        std::fs::write(&exe, b"build-one").unwrap();
        set_executable(&exe);
        let start = FileIdentity::capture(&exe).unwrap();

        std::fs::remove_file(&exe).unwrap();
        std::fs::write(&exe, b"build-two-is-longer").unwrap();
        set_executable(&exe);

        assert!(
            exe_replaced(&start, &exe),
            "a rm+cp replacement (different length) must be detected"
        );
    }

    #[test]
    fn exe_replaced_false_for_an_untouched_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let exe = tmp.path().join("csr-engine");
        std::fs::write(&exe, b"build-one").unwrap();
        set_executable(&exe);
        let start = FileIdentity::capture(&exe).unwrap();

        assert!(
            !exe_replaced(&start, &exe),
            "an untouched binary must not look like an upgrade"
        );
    }

    /// Same inode, same length, different bytes and a forced mtime/ctime
    /// change — the in-place-overwrite case an ino-only identity would
    /// miss (gap-G2 review finding 3).
    #[test]
    fn exe_replaced_true_for_an_in_place_overwrite_same_inode_same_length() {
        let tmp = tempfile::TempDir::new().unwrap();
        let exe = tmp.path().join("csr-engine");
        std::fs::write(&exe, b"AAAAAAAAAA").unwrap();
        set_executable(&exe);
        let start = FileIdentity::capture(&exe).unwrap();

        // Sleep past filesystem mtime granularity before rewriting in place.
        std::thread::sleep(Duration::from_millis(1100));
        std::fs::write(&exe, b"BBBBBBBBBB").unwrap(); // same length, same inode
        set_executable(&exe);

        assert!(
            exe_replaced(&start, &exe),
            "an in-place overwrite (same inode, same length) must still be caught by mtime/ctime"
        );
    }

    #[test]
    fn exe_replaced_fails_closed_when_the_file_is_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let exe = tmp.path().join("csr-engine");
        std::fs::write(&exe, b"build-one").unwrap();
        let start = FileIdentity::capture(&exe).unwrap();
        std::fs::remove_file(&exe).unwrap();

        assert!(
            !exe_replaced(&start, &exe),
            "an unreadable current file must report 'not replaced', not a false positive"
        );
    }

    /// Would fail if `exe_replaced` were hard-coded to `false` — guards
    /// against a stubbed-out detector shipping unnoticed.
    #[test]
    fn exe_replaced_is_not_a_constant() {
        let tmp = tempfile::TempDir::new().unwrap();
        let exe = tmp.path().join("csr-engine");
        std::fs::write(&exe, b"one").unwrap();
        let start = FileIdentity::capture(&exe).unwrap();
        std::fs::remove_file(&exe).unwrap();
        std::fs::write(&exe, b"a-different-length-value").unwrap();
        assert!(exe_replaced(&start, &exe));
    }

    // ── poll_replacement / wait_for_binary_replacement (spec test 3(iv)) ──

    #[test]
    fn poll_replacement_requires_two_consecutive_agreeing_polls() {
        let tmp = tempfile::TempDir::new().unwrap();
        let exe = tmp.path().join("csr-engine");
        std::fs::write(&exe, b"build-one").unwrap();
        set_executable(&exe);
        let start = FileIdentity::capture(&exe).unwrap();

        std::fs::remove_file(&exe).unwrap();
        std::fs::write(&exe, b"build-two-is-longer").unwrap();
        set_executable(&exe);

        let (confirmed_first, seen) = poll_replacement(&start, None, &exe);
        assert!(
            !confirmed_first,
            "a single sighting must not confirm — could be mid-install"
        );
        assert!(seen.is_some());

        let (confirmed_second, _) = poll_replacement(&start, seen, &exe);
        assert!(
            confirmed_second,
            "the same identity observed twice in a row must confirm"
        );
    }

    #[test]
    fn poll_replacement_rejects_a_non_executable_candidate() {
        let tmp = tempfile::TempDir::new().unwrap();
        let exe = tmp.path().join("csr-engine");
        std::fs::write(&exe, b"build-one").unwrap();
        set_executable(&exe);
        let start = FileIdentity::capture(&exe).unwrap();

        std::fs::remove_file(&exe).unwrap();
        std::fs::write(&exe, b"partial-install-write").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        #[cfg(unix)]
        {
            let (confirmed, seen) = poll_replacement(&start, None, &exe);
            assert!(!confirmed);
            assert!(
                seen.is_none(),
                "a non-executable candidate must not even start the confirm window"
            );
        }
    }

    #[test]
    fn poll_replacement_rejects_an_empty_candidate() {
        let tmp = tempfile::TempDir::new().unwrap();
        let exe = tmp.path().join("csr-engine");
        std::fs::write(&exe, b"build-one").unwrap();
        set_executable(&exe);
        let start = FileIdentity::capture(&exe).unwrap();

        std::fs::remove_file(&exe).unwrap();
        std::fs::write(&exe, b"").unwrap(); // truncated mid-copy
        set_executable(&exe);

        let (confirmed, seen) = poll_replacement(&start, None, &exe);
        assert!(!confirmed);
        assert!(
            seen.is_none(),
            "a zero-length candidate must not start the confirm window"
        );
    }

    /// The daemon-loop seam: proves the async wait resolves (and therefore
    /// the daemon maps to `DAEMON_REPLACED_EXIT_CODE`) once a replacement
    /// is confirmed, using the exact function `Daemon::run` calls.
    #[tokio::test]
    async fn wait_for_binary_replacement_resolves_after_confirmation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let exe = tmp.path().join("csr-engine");
        std::fs::write(&exe, b"build-one").unwrap();
        set_executable(&exe);
        let start = FileIdentity::capture(&exe).unwrap();

        std::fs::remove_file(&exe).unwrap();
        std::fs::write(&exe, b"build-two-is-longer").unwrap();
        set_executable(&exe);

        let outcome = tokio::time::timeout(
            Duration::from_secs(2),
            wait_for_binary_replacement(
                start,
                exe.clone(),
                Duration::from_millis(5),
                Duration::from_millis(5),
            ),
        )
        .await;
        assert!(
            outcome.is_ok(),
            "must confirm the replacement well within the timeout"
        );
        // This is exactly what `Daemon::run` does once the wait above resolves.
        let exit_code = DAEMON_REPLACED_EXIT_CODE;
        assert_eq!(exit_code, 75);
    }

    #[tokio::test]
    async fn wait_for_binary_replacement_never_resolves_for_an_untouched_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let exe = tmp.path().join("csr-engine");
        std::fs::write(&exe, b"build-one").unwrap();
        set_executable(&exe);
        let start = FileIdentity::capture(&exe).unwrap();

        let outcome = tokio::time::timeout(
            Duration::from_millis(50),
            wait_for_binary_replacement(
                start,
                exe,
                Duration::from_millis(5),
                Duration::from_millis(5),
            ),
        )
        .await;
        assert!(outcome.is_err(), "an untouched binary must never confirm");
    }

    // ── MCP notice helper (spec test 3(iii)) ──

    #[test]
    fn append_binary_replaced_notice_adds_nothing_when_not_replaced() {
        let result =
            rmcp::model::CallToolResult::success(vec![rmcp::model::ContentBlock::text("ok")]);
        let before = result.content.len();
        let after = append_binary_replaced_notice(result, None);
        assert_eq!(after.content.len(), before);
    }

    #[test]
    fn append_binary_replaced_notice_adds_one_block_when_replaced() {
        let result =
            rmcp::model::CallToolResult::success(vec![rmcp::model::ContentBlock::text("ok")]);
        let before = result.content.len();
        let after =
            append_binary_replaced_notice(result, Some(replacement_notice("2026-09-22T19:40:00Z")));
        assert_eq!(after.content.len(), before + 1);
        let rmcp::model::ContentBlock::Text(text) = after.content.last().unwrap() else {
            panic!("expected a text content block");
        };
        assert!(text.text.contains("<notice>"));
        assert!(text.text.contains("2026-09-22T19:40:00Z"));
    }

    #[test]
    fn replacement_watch_reports_no_notice_before_a_replacement() {
        let tmp = tempfile::TempDir::new().unwrap();
        let exe = tmp.path().join("csr-engine");
        std::fs::write(&exe, b"build-one").unwrap();
        set_executable(&exe);
        let watch = ReplacementWatch {
            start: FileIdentity::capture(&exe),
            exe_path: Some(exe),
            last_checked_secs: AtomicI64::new(i64::MIN),
            replaced: AtomicBool::new(false),
            replaced_mtime: Mutex::new(None),
        };
        assert!(watch.check_notice(Duration::from_secs(30)).is_none());
        assert!(!watch.is_replaced());
    }

    #[test]
    fn replacement_watch_reports_a_notice_once_replaced_and_caches_it() {
        let tmp = tempfile::TempDir::new().unwrap();
        let exe = tmp.path().join("csr-engine");
        std::fs::write(&exe, b"build-one").unwrap();
        set_executable(&exe);
        let start = FileIdentity::capture(&exe);

        std::fs::remove_file(&exe).unwrap();
        std::fs::write(&exe, b"build-two-is-longer").unwrap();
        set_executable(&exe);

        let watch = ReplacementWatch {
            start,
            exe_path: Some(exe.clone()),
            last_checked_secs: AtomicI64::new(i64::MIN),
            replaced: AtomicBool::new(false),
            replaced_mtime: Mutex::new(None),
        };
        let notice = watch.check_notice(Duration::from_secs(30));
        assert!(notice.is_some());
        assert!(watch.is_replaced());

        // Delete the file entirely — a second call within the interval must
        // return the cached notice, not re-stat and fail closed.
        std::fs::remove_file(&exe).unwrap();
        let cached = watch.check_notice(Duration::from_secs(30));
        assert_eq!(cached, notice);
    }

    // ── Run registry (spec test 3(ii)) ──

    #[test]
    fn stale_processes_reports_mismatched_live_pid_and_cleans_dead_pid() {
        let tmp = tempfile::TempDir::new().unwrap();
        let run_dir = tmp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();

        // Two distinct on-disk identities: the "current" build, and an
        // "old" one that a stale record points at.
        let current_file = tmp.path().join("current-binary");
        std::fs::write(&current_file, b"current-build").unwrap();
        let current = FileIdentity::capture(&current_file).unwrap();

        let old_file = tmp.path().join("old-binary");
        std::fs::write(&old_file, b"an-older-build-different-length").unwrap();
        let old_identity = FileIdentity::capture(&old_file).unwrap();

        // A genuinely dead pid: spawn a trivial child and wait for it to exit.
        let mut child = std::process::Command::new("true")
            .spawn()
            .or_else(|_| {
                std::process::Command::new("cmd")
                    .args(["/C", "exit", "0"])
                    .spawn()
            })
            .expect("spawn a short-lived child process");
        let dead_pid = child.id();
        child.wait().unwrap();

        let live_record = RunRecord {
            pid: std::process::id(),
            version: "10.1.0".into(),
            git_sha: "deadbeef".into(),
            exe_identity: old_identity,
            role: "mcp_stdio".into(),
            started_at: "2026-09-22T10:06:00Z".into(),
            parent_cmd: Some("claude".into()),
        };
        let dead_record = RunRecord {
            pid: dead_pid,
            version: "10.1.0".into(),
            git_sha: "deadbeef".into(),
            exe_identity: old_identity,
            role: "mcp_stdio".into(),
            started_at: "2026-09-22T10:06:00Z".into(),
            parent_cmd: Some("claude".into()),
        };
        std::fs::write(
            run_dir.join(format!("{}.json", live_record.pid)),
            serde_json::to_string(&live_record).unwrap(),
        )
        .unwrap();
        let dead_path = run_dir.join(format!("{}.json", dead_record.pid));
        std::fs::write(&dead_path, serde_json::to_string(&dead_record).unwrap()).unwrap();

        let stale = stale_processes(&run_dir, &current);

        assert_eq!(
            stale.len(),
            1,
            "exactly one live, mismatched process: {stale:?}"
        );
        assert_eq!(stale[0].pid, std::process::id());
        assert_eq!(stale[0].role, "mcp_stdio");
        assert!(
            !dead_path.exists(),
            "the dead pid's record must be cleaned up"
        );
    }

    #[test]
    fn stale_processes_reports_nothing_for_a_matching_identity() {
        let tmp = tempfile::TempDir::new().unwrap();
        let run_dir = tmp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();

        let current_file = tmp.path().join("current-binary");
        std::fs::write(&current_file, b"current-build").unwrap();
        let current = FileIdentity::capture(&current_file).unwrap();

        let record = RunRecord {
            pid: std::process::id(),
            version: "10.1.0".into(),
            git_sha: "deadbeef".into(),
            exe_identity: current,
            role: "daemon".into(),
            started_at: "2026-09-22T10:06:00Z".into(),
            parent_cmd: None,
        };
        std::fs::write(
            run_dir.join(format!("{}.json", record.pid)),
            serde_json::to_string(&record).unwrap(),
        )
        .unwrap();

        assert!(stale_processes(&run_dir, &current).is_empty());
    }

    #[test]
    fn write_and_remove_run_record_round_trips() {
        let tmp = tempfile::TempDir::new().unwrap();
        let run_dir = tmp.path().join("run");
        let identity = current_exe_identity().unwrap_or(FileIdentity {
            #[cfg(unix)]
            dev: 0,
            #[cfg(unix)]
            ino: 0,
            len: 1,
            mtime_ns: 1,
            #[cfg(unix)]
            ctime_ns: 1,
            #[cfg(windows)]
            creation_time: 1,
        });
        let path = write_run_record(&run_dir, "mcp_stdio", identity).expect("write should succeed");
        assert!(path.exists());
        let record: RunRecord = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(record.pid, std::process::id());
        assert_eq!(record.role, "mcp_stdio");
        assert_eq!(record.version, env!("CARGO_PKG_VERSION"));

        remove_run_record(&path);
        assert!(!path.exists());
    }
}
