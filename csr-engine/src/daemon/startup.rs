//! Small daemon startup helpers.

/// Page cache ceiling for each of the daemon's SQLite connections: the main
/// one and the prompt lane's own. The ceiling fills with pages a connection
/// reads past the 2 GiB map, so two connections at the default 64 MiB could
/// hold about 128 MiB of dirty heap between them.
pub const DAEMON_READ_CACHE_KIB: i64 = 16_384;

/// Give the connections this process opens from now on the daemon's page
/// cache ceiling. Call it before the first `Storage::open`, as `main` does for
/// a proxied MCP server. `CSR_SQLITE_CACHE_KIB` still wins over it. Only this
/// process changes: children the daemon spawns, hooks and the pane keep theirs.
pub fn apply_daemon_read_cache() {
    crate::storage::set_read_cache_kib(DAEMON_READ_CACHE_KIB);
}

/// `CSR_NO_CODEX_IMPORT` kill switch — same "1"/"true" (case-insensitive) idiom
/// as `memory_registry_disabled`. Skips the optional `~/.codex/sessions` rollout
/// import in both the daemon loop and the `setup` one-shot: the first pass over
/// a large Codex history takes hours and holds the daemon's heavy-work permit,
/// which is the one thing a user starting the daemon for dreaming did not ask for.
/// Every other subsystem already had a switch; this was the last one without.
pub fn codex_import_disabled() -> bool {
    std::env::var("CSR_NO_CODEX_IMPORT")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Storage;

    const CHILD_FLAG: &str = "CSR_TEST_DAEMON_CACHE_CHILD";
    const DAEMON_TEST: &str =
        "daemon::startup::tests::a_daemon_opens_its_connections_with_a_16_mib_cache";

    fn cache_kib(storage: &Storage) -> i64 {
        storage
            .with_connection(|conn| Ok(conn.query_row("PRAGMA cache_size", [], |r| r.get(0))?))
            .unwrap()
    }

    fn mmap_bytes(storage: &Storage) -> i64 {
        storage
            .with_connection(|conn| Ok(conn.query_row("PRAGMA mmap_size", [], |r| r.get(0))?))
            .unwrap()
    }

    /// `apply_daemon_read_cache` writes a process-wide setting, so the check
    /// runs in a child copy of this test binary: no other test in the parent
    /// process can open a store while the setting is changed.
    #[test]
    fn a_daemon_opens_its_connections_with_a_16_mib_cache() {
        if std::env::var_os(CHILD_FLAG).is_none() {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", DAEMON_TEST, "--test-threads=1"])
                .env(CHILD_FLAG, "1")
                .env_remove("CSR_SQLITE_CACHE_KIB")
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(
                out.status.success() && stdout.contains("1 passed"),
                "child run failed: {stdout}\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        assert_eq!(DAEMON_READ_CACHE_KIB, 16_384);
        let dir = tempfile::tempdir().unwrap();
        apply_daemon_read_cache();
        let storage = Storage::open(&dir.path().join("csr.db")).unwrap();
        assert_eq!(cache_kib(&storage), -16_384, "main connection");
        assert_eq!(mmap_bytes(&storage), 2_147_418_112, "mmap is not changed");
        let lane = storage.open_prompt_lane().unwrap().expect("a file store");
        assert_eq!(cache_kib(&lane), -16_384, "prompt lane connection");
        let reader = storage.open_read_lane().unwrap().expect("a file store");
        assert_eq!(cache_kib(&reader), -16_384, "read lane connection");
    }

    /// Every process but the daemon keeps the 64 MiB default. This test does
    /// not call the setter, so it reads the default whatever the daemon does.
    #[test]
    fn a_default_open_still_reads_the_64_mib_cache() {
        if std::env::var_os("CSR_SQLITE_CACHE_KIB").is_some() {
            return;
        }
        assert_eq!(crate::storage::DEFAULT_READ_CACHE_KIB, 65_536);
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("csr.db")).unwrap();
        assert_eq!(cache_kib(&storage), -65_536);
        assert_eq!(mmap_bytes(&storage), 2_147_418_112);
        let lane = storage.open_prompt_lane().unwrap().expect("a file store");
        assert_eq!(cache_kib(&lane), -65_536);
    }
}
