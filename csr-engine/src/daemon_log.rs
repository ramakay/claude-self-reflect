//! Start-time rotation of the daemon's stderr log.
//!
//! launchd points the daemon's stderr (`StandardErrorPath`, `daemon.err.log`) at
//! a plain file that nothing ever trims. When the daemon starts, [`rotate_stderr_file`]
//! checks it: if fd 2 is a regular file over [`LOG_CAP_BYTES`], the file is renamed
//! to `<name>.1` (one generation; an older `.1` is replaced) and fd 2 is reopened
//! on a fresh file of the same name with `dup2`. fd 1 follows when it points at
//! the same file. After that the daemon writes straight to the file as before: no
//! pipe, no thread, no change to any line.
//!
//! Only the daemon entry calls this. Hooks, the CLI and the MCP server never do.
//! A terminal, a pipe or a file under the cap is left alone.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

/// A log over this many bytes is rotated at daemon start: 10 MiB.
pub const LOG_CAP_BYTES: u64 = 10 * 1024 * 1024;
/// The rotated copy sits beside the log under this suffix. One generation is
/// kept, so the log and its copy together stay near twice the cap.
pub const ROTATED_SUFFIX: &str = ".1";

/// Whether a log of `len` bytes is over `cap` and due for rotation.
pub fn should_rotate(len: u64, cap: u64) -> bool {
    len > cap
}

/// Where the rotated copy of `path` goes: `path` with [`ROTATED_SUFFIX`] added.
pub fn rotated_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(ROTATED_SUFFIX);
    PathBuf::from(name)
}

/// Rotate the file fd 2 points at when it is over [`LOG_CAP_BYTES`]. A failure
/// is reported on stderr and the daemon goes on writing to the file it has.
pub fn rotate_stderr_file() {
    #[cfg(unix)]
    {
        let stamp = crate::telemetry::stamp_timing_line;
        match unix::rotate_fd(libc::STDERR_FILENO, libc::STDOUT_FILENO, LOG_CAP_BYTES) {
            Ok(Some(path)) => eprintln!(
                "{}",
                stamp(&format!(
                    "CSR daemon log: the stderr log was over {} MiB; the old one is {:?}",
                    LOG_CAP_BYTES / (1024 * 1024),
                    rotated_path(&path)
                ))
            ),
            Ok(None) => {}
            Err(why) => eprintln!(
                "{}",
                stamp(&format!("CSR daemon log: rotation skipped: {why}"))
            ),
        }
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::mem::ManuallyDrop;
    use std::os::fd::{AsRawFd, FromRawFd, RawFd};
    use std::os::unix::fs::MetadataExt;

    /// The path fd `fd` was opened from.
    #[cfg(target_os = "macos")]
    fn fd_path(fd: RawFd) -> Option<PathBuf> {
        use std::os::unix::ffi::OsStrExt;
        let mut buf = [0 as libc::c_char; libc::PATH_MAX as usize];
        // SAFETY: F_GETPATH writes a NUL-terminated path of at most PATH_MAX bytes.
        if unsafe { libc::fcntl(fd, libc::F_GETPATH, buf.as_mut_ptr()) } != 0 {
            return None;
        }
        // SAFETY: the call above NUL-terminated `buf`.
        let text = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) };
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(text.to_bytes())))
    }

    #[cfg(target_os = "linux")]
    fn fd_path(fd: RawFd) -> Option<PathBuf> {
        std::fs::read_link(format!("/proc/self/fd/{fd}")).ok()
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn fd_path(_fd: RawFd) -> Option<PathBuf> {
        None
    }

    /// The (device, inode) and length of the file behind `fd`, without closing it.
    fn identity(fd: RawFd) -> std::io::Result<(u64, u64, u64, bool)> {
        // SAFETY: the handle never owns `fd`; ManuallyDrop keeps it from closing it.
        let file = ManuallyDrop::new(unsafe { File::from_raw_fd(fd) });
        let meta = file.metadata()?;
        Ok((
            meta.dev(),
            meta.ino(),
            meta.len(),
            meta.file_type().is_file(),
        ))
    }

    /// Rotate the regular file behind `target` when it is over `cap`: rename it to
    /// its `.1` copy, open a fresh file at the old path and `dup2` it onto
    /// `target`, and onto `also` when `also` pointed at the same file. Returns the
    /// path that was rotated, or None when nothing was due. Takes the fds as
    /// arguments so a test can use spare ones.
    pub(super) fn rotate_fd(
        target: RawFd,
        also: RawFd,
        cap: u64,
    ) -> Result<Option<PathBuf>, String> {
        let (dev, ino, len, is_file) = identity(target).map_err(|e| format!("stat: {e}"))?;
        if !is_file || !should_rotate(len, cap) {
            return Ok(None);
        }
        let path = fd_path(target).ok_or("the path of the log is unknown")?;
        let at_path = std::fs::metadata(&path).map_err(|e| format!("stat of {path:?}: {e}"))?;
        if (at_path.dev(), at_path.ino()) != (dev, ino) {
            return Err(format!("the log is no longer the file at {path:?}"));
        }
        let follows = also != target
            && identity(also).is_ok_and(|(d, i, _, is_file)| is_file && (d, i) == (dev, ino));
        std::fs::rename(&path, rotated_path(&path))
            .map_err(|e| format!("rename of {path:?}: {e}"))?;
        let fresh = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("open of {path:?}: {e}"))?;
        for fd in [Some(target), follows.then_some(also)]
            .into_iter()
            .flatten()
        {
            // SAFETY: `fresh` is open and `fd` is a descriptor of this process.
            if unsafe { libc::dup2(fresh.as_raw_fd(), fd) } < 0 {
                return Err(format!(
                    "dup2 onto fd {fd}: {}",
                    std::io::Error::last_os_error()
                ));
            }
        }
        Ok(Some(path))
    }
}

#[cfg(test)]
#[path = "daemon_log_tests.rs"]
mod tests;
