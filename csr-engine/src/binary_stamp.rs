//! Detect when the `csr-engine` binary on disk has been replaced while an MCP
//! server started from the *previous* build is still serving the session.
//!
//! Why this is detection and not reload: the MCP server speaks stdio, and the
//! protocol's `initialize` handshake is performed once by the client. A running
//! server cannot swap itself for a newer build transparently — `exec` would
//! preserve the pipe but not the negotiated session, and the client has no
//! obligation to re-initialize on demand. So the honest thing a server CAN do
//! is make its own staleness visible, turning "did my upgrade take effect?"
//! into an observable fact instead of a guess.
//!
//! Mechanism: the serving process records a fingerprint of its own executable
//! at startup. `status` runs as a separate process, reads that record, and
//! compares it against the binary currently on disk. A difference means a new
//! build was installed under a live server, and the MCP connection needs to be
//! re-established for it to take effect.

use std::path::{Path, PathBuf};

/// Where the live MCP server records which binary it is running.
fn stamp_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude-self-reflect").join("mcp-binary.txt"))
}

/// Identity of an executable that is cheap to compute and changes on reinstall:
/// modification time in nanos plus byte length. Deliberately not a content hash
/// — this runs on the status path, and rehashing ~59MB to render a statusline
/// would cost more than the signal is worth.
fn fingerprint(path: &Path) -> Option<String> {
    let md = std::fs::metadata(path).ok()?;
    let mtime = md
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(format!("{mtime}:{}", md.len()))
}

/// Fingerprint of the executable backing the current process.
fn current_fingerprint() -> Option<String> {
    fingerprint(&std::env::current_exe().ok()?)
}

/// Record the binary this MCP server is serving from. Best-effort and silent:
/// a missing home dir or unwritable state dir must never affect serving.
pub fn record_serving_binary() {
    let (Some(path), Some(fp)) = (stamp_path(), current_fingerprint()) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, format!("{}\n{fp}\n", std::process::id()));
}

/// True when a live MCP server recorded a different binary than the one now on
/// disk — i.e. an upgrade landed underneath it and a reconnect is required.
///
/// Fails closed (returns false) whenever the answer is not knowable: no stamp
/// yet, unreadable state, or an executable path that cannot be resolved. A
/// false "up to date" is a silent no-op; a false "stale" would send someone
/// chasing a reconnect they do not need.
pub fn serving_binary_is_stale() -> bool {
    let Some(path) = stamp_path() else {
        return false;
    };
    let Ok(recorded) = std::fs::read_to_string(&path) else {
        return false;
    };
    let Some(recorded_fp) = recorded.lines().nth(1) else {
        return false;
    };
    let Some(current) = current_fingerprint() else {
        return false;
    };
    !recorded_fp.trim().is_empty() && recorded_fp.trim() != current
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_changes_when_the_file_is_replaced() {
        let tmp = tempfile::TempDir::new().unwrap();
        let exe = tmp.path().join("csr-engine");
        std::fs::write(&exe, b"build-one").unwrap();
        let first = fingerprint(&exe).expect("fingerprint of an existing file");

        // A reinstall writes different bytes; length alone must already differ.
        std::fs::write(&exe, b"build-two-which-is-longer").unwrap();
        let second = fingerprint(&exe).expect("fingerprint after replacement");

        assert_ne!(
            first, second,
            "a replaced binary must not fingerprint identically"
        );
    }

    #[test]
    fn fingerprint_is_stable_for_an_untouched_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let exe = tmp.path().join("csr-engine");
        std::fs::write(&exe, b"build-one").unwrap();

        assert_eq!(
            fingerprint(&exe),
            fingerprint(&exe),
            "an untouched binary must not look like an upgrade"
        );
    }

    #[test]
    fn missing_file_has_no_fingerprint() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(fingerprint(&tmp.path().join("absent")).is_none());
    }
}
