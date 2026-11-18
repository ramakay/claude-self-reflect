//! `csr-engine daemon` re-executes itself once with macOS malloc's large-block
//! cache off; no other entry point does. These run the real binary against a
//! temp HOME. The daemon cases hold the daemon lock first, so the daemon exits
//! right after the start-up step under test, before it opens anything.
#![cfg(target_os = "macos")]

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use csr_engine::daemon_start::{GUARD, OFF_SWITCH, STARTED_LINE, VARIABLE};

fn engine(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_csr-engine"));
    command
        .arg("--db-path")
        .arg(home.join(".claude-self-reflect/csr-engine.db"))
        .arg("--projects-dir")
        .arg(home.join("projects"))
        .env("HOME", home)
        .env_remove(VARIABLE)
        .env_remove(GUARD)
        .env_remove(OFF_SWITCH)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    command
}

/// Wait up to `limit` for the child to exit (killing it after), then return
/// its stderr.
fn stderr_within(mut child: Child, limit: Duration) -> String {
    let deadline = Instant::now() + limit;
    while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    let mut text = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    text
}

/// Run `csr-engine daemon` while another holder has the daemon lock.
fn losing_daemon(home: &Path, off: Option<&str>) -> String {
    use fs2::FileExt;
    let data = home.join(".claude-self-reflect");
    std::fs::create_dir_all(&data).unwrap();
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(data.join("daemon.lock"))
        .unwrap();
    FileExt::try_lock_exclusive(&lock).unwrap();
    let mut command = engine(home);
    command
        .arg("daemon")
        .env_remove("CSR_NO_DAEMON")
        .stdin(Stdio::null());
    if let Some(off) = off {
        command.env(OFF_SWITCH, off);
    }
    let stderr = stderr_within(command.spawn().unwrap(), Duration::from_secs(20));
    assert!(stderr.contains("another daemon instance"), "{stderr}");
    drop(lock);
    stderr
}

#[test]
fn the_daemon_re_executes_once_with_the_large_block_cache_off() {
    let home = tempfile::tempdir().unwrap();
    let stderr = losing_daemon(home.path(), None);
    assert_eq!(stderr.matches(STARTED_LINE).count(), 1, "{stderr}");
}

/// The daemon log is a plain file: a line without a time cannot be placed. The start line
/// carries the same `<UTC second> ` stamp as the hook-timing log.
#[test]
fn the_daemon_start_line_is_stamped_with_utc_time() {
    let home = tempfile::tempdir().unwrap();
    let stderr = losing_daemon(home.path(), None);
    let line = stderr
        .lines()
        .find(|line| line.contains(STARTED_LINE))
        .unwrap_or_else(|| panic!("no start line\n{stderr}"));
    let stamp = line.split(' ').next().unwrap();
    assert!(
        stamp.len() == 20
            && stamp.ends_with('Z')
            && stamp.as_bytes()[10] == b'T'
            && chrono::DateTime::parse_from_rfc3339(stamp).is_ok(),
        "{line}"
    );
}

#[test]
fn the_kill_switch_keeps_the_daemon_as_started() {
    let home = tempfile::tempdir().unwrap();
    let stderr = losing_daemon(home.path(), Some("1"));
    assert!(!stderr.contains("CSR daemon malloc"), "{stderr}");
}

#[test]
fn the_prompt_hook_and_the_mcp_server_never_set_it() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".claude-self-reflect")).unwrap();
    let mut hook = engine(home.path())
        .args(["hook", "prompt-submit"])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let input = serde_json::json!({
        "prompt": "how does the daemon schedule enrichment",
        "cwd": home.path(),
        "session_id": "malloc-test",
    });
    hook.stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    let stderr = stderr_within(hook, Duration::from_secs(10));
    assert!(!stderr.contains("CSR daemon malloc"), "{stderr}");

    // The MCP server is the default command. Its start-up step would print
    // within milliseconds; it is stopped long before it could load a model.
    let mcp = engine(home.path()).stdin(Stdio::piped()).spawn().unwrap();
    let stderr = stderr_within(mcp, Duration::from_secs(2));
    assert!(!stderr.contains("CSR daemon malloc"), "{stderr}");
}
