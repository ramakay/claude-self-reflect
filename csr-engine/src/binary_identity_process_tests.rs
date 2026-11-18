use super::*;
use crate::process::EngineProbe;

#[test]
fn registry_unknown_probes_are_batched_kept_and_omitted() {
    let dir = tempfile::tempdir().unwrap();
    let identity = current_exe_identity().unwrap();
    let paths = [
        write_record(dir.path(), 123, identity),
        write_record(dir.path(), 456, identity),
    ];
    let probe = |pids: &[u32]| {
        let mut sorted = pids.to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted, [123, 456], "probe the whole registry once");
        pids.iter()
            .map(|&pid| (pid, EngineProbe::Unknown))
            .collect()
    };
    assert!(live_mcp_servers_with(dir.path(), probe).is_empty());
    assert!(stale_processes_with(dir.path(), &identity, probe).is_empty());
    assert!(paths.iter().all(|path| path.exists()));
}

#[test]
fn registry_confirmed_non_engine_is_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let identity = current_exe_identity().unwrap();
    let path = write_record(dir.path(), 123, identity);
    let probe = |pids: &[u32]| {
        assert_eq!(pids, [123]);
        [(123, EngineProbe::NotEngine)].into_iter().collect()
    };
    assert!(live_mcp_servers_with(dir.path(), probe).is_empty());
    assert!(path.exists(), "listing must not remove records");
    assert!(stale_processes_with(dir.path(), &identity, probe).is_empty());
    assert!(!path.exists());
}

fn write_record(dir: &Path, pid: u32, identity: FileIdentity) -> PathBuf {
    let path = dir.join(format!("{pid}.json"));
    let record = RunRecord {
        pid,
        version: "test".into(),
        git_sha: "test".into(),
        exe_identity: identity,
        role: "mcp_stdio".into(),
        started_at: "2026-11-09T00:00:00Z".into(),
        parent_cmd: None,
        search_path: None,
    };
    std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    path
}

#[test]
fn registry_rejects_a_live_unrelated_process_even_with_a_record() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("old");
    let current = dir.path().join("current");
    std::fs::write(&old, b"old").unwrap();
    std::fs::write(&current, b"current image").unwrap();
    let path = write_record(
        dir.path(),
        std::process::id(),
        FileIdentity::capture(&old).unwrap(),
    );
    assert!(live_mcp_servers(dir.path()).is_empty());
    assert!(stale_processes(dir.path(), &FileIdentity::capture(&current).unwrap()).is_empty());
    assert!(!path.exists(), "a recycled registry entry must be removed");
    registry_rejects_an_exited_pid();
    registry_preserves_a_live_engine_with_a_superseded_identity();
}

fn registry_rejects_an_exited_pid() {
    let dir = tempfile::tempdir().unwrap();
    let identity = current_exe_identity().unwrap();
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    let path = write_record(dir.path(), pid, identity);
    assert!(live_mcp_servers(dir.path()).is_empty());
    assert!(stale_processes(dir.path(), &identity).is_empty());
    assert!(!path.exists());
}

fn registry_preserves_a_live_engine_with_a_superseded_identity() {
    let dir = tempfile::tempdir().unwrap();
    // The OS reports the executable name, independently of the registry.
    let executable = dir.path().join("csr-engine");
    std::fs::copy("/bin/sleep", &executable).unwrap();
    let old = dir.path().join("old");
    std::fs::write(&old, b"superseded image").unwrap();
    let old_identity = FileIdentity::capture(&old).unwrap();
    let current = FileIdentity::capture(&executable).unwrap();
    let mut child = std::process::Command::new(&executable)
        .arg("2")
        .spawn()
        .unwrap();
    let path = write_record(dir.path(), child.id(), old_identity);
    // Always reap our own child, including when an assertion below fails.
    let servers = live_mcp_servers(dir.path());
    let stale = stale_processes(dir.path(), &current);
    child.wait().unwrap();
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].pid, child.id());
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].pid, child.id());
    assert!(path.exists());
}
