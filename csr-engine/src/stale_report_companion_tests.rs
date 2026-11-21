use super::*;
use crate::binary_identity::StaleProcess;

const EXE: &str = "/usr/local/bin/csr-engine";
const DISK: u64 = 9;

/// A daemon, an MCP server, two panes (one dormant), two relays and an unrelated binary.
fn ps() -> String {
    [
        "11792     1 /usr/local/bin/csr-engine daemon",
        "26149 26135 /usr/local/bin/csr-engine",
        "26208 26135 /usr/local/bin/csr-engine companion serve --sock /h/run/0739ec61-mwaa49xt.sock --relay 10.1.0",
        "44101 43668 /usr/local/bin/csr-engine companion pane --repo /p/web --watch --sid e22fe55d-a6d6-40a9-b72c-040597c48324 --dormant",
        "44102 43668 /usr/local/bin/csr-engine companion pane --repo /p/api --watch --sid 5b1c9a77-0000-4000-8000-000000000001",
        "43500 44522 /usr/local/bin/csr-engine companion serve --sock /h/run/e22fe55d-mwb0r536.sock --relay 10.1.0",
        "500 1 /usr/local/bin/csr-engine-other companion pane --repo /p/x",
    ]
    .join("\n")
}

fn stale(txt: &HashMap<u32, u64>) -> Vec<StaleProcess> {
    stale_companions_from(&ps(), EXE, &|_| false, txt, DISK)
}

fn pid_role(list: &[StaleProcess]) -> Vec<(u32, &str)> {
    list.iter().map(|p| (p.pid, p.role.as_str())).collect()
}

#[test]
fn panes_and_relays_on_an_older_inode_are_stale_but_the_daemon_and_mcp_are_not_added() {
    // Every process maps an older inode than the one on disk.
    let txt: HashMap<u32, u64> = [11792, 26149, 26208, 44101, 44102, 43500]
        .into_iter()
        .map(|pid| (pid, 7))
        .collect();
    assert_eq!(
        pid_role(&stale(&txt)),
        vec![
            (26208, COMPANION_SERVE_ROLE),
            (43500, COMPANION_SERVE_ROLE),
            (44101, COMPANION_PANE_ROLE),
            (44102, COMPANION_PANE_ROLE),
        ]
    );
}

#[test]
fn a_pane_on_the_current_inode_or_with_no_readable_inode_is_not_claimed() {
    // 44101 current, 44102 unreadable (absent), the two relays stale.
    let txt = HashMap::from([(44101, DISK), (26208, 7), (43500, 7)]);
    assert_eq!(
        pid_role(&stale(&txt)),
        vec![(26208, COMPANION_SERVE_ROLE), (43500, COMPANION_SERVE_ROLE)]
    );
    assert!(stale(&HashMap::new()).is_empty());
}

#[test]
fn a_companion_never_takes_the_mcp_stdio_role_that_sets_mcp_binary_stale() {
    let txt = HashMap::from([(44101, 7), (26208, 7)]);
    assert!(stale(&txt).iter().all(|p| p.role != "mcp_stdio"));
    assert!(stale(&txt).iter().all(|p| p.started_at == "unknown"));
}

#[test]
fn merge_keeps_the_registry_entry_and_adds_only_new_pids() {
    let registry = vec![StaleProcess {
        pid: 44101,
        role: "mcp_stdio".into(),
        started_at: "2026-11-09T00:00:00Z".into(),
        parent: None,
    }];
    let txt = HashMap::from([(44101, 7), (44102, 7)]);
    let merged = merge_companions(registry, stale(&txt));
    assert_eq!(
        pid_role(&merged),
        vec![(44101, "mcp_stdio"), (44102, COMPANION_PANE_ROLE)]
    );
    assert_eq!(merged[0].started_at, "2026-11-09T00:00:00Z");
}

#[test]
fn a_read_that_outlives_its_deadline_gives_nothing_and_is_not_left_running() {
    use std::time::{Duration, Instant};
    let started = Instant::now();
    assert_eq!(
        text_within("sleep", &["5"], Duration::from_millis(200)),
        None
    );
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "waited {:?} on a 5 s sleep",
        started.elapsed()
    );
}

#[test]
fn a_child_that_closes_stdout_and_keeps_running_is_bounded_and_reaped() {
    use std::time::{Duration, Instant};
    let sh = |script: &str| {
        Command::new("sh")
            .args(["-c", script])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap()
    };
    // End of output, but the process runs on: text_within must not wait for it.
    let started = Instant::now();
    assert_eq!(
        text_within(
            "sh",
            &["-c", "exec >&-; sleep 5"],
            Duration::from_millis(300)
        ),
        None
    );
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    // The helper kills and reaps its own child: nothing is left to wait for.
    let mut child = sh("exec >&-; sleep 5");
    assert!(!exited_by(
        &mut child,
        Instant::now() + Duration::from_millis(200)
    ));
    assert!(child.try_wait().unwrap().is_some(), "left a zombie");
    // A child that does exit in time is not killed.
    let mut quick = sh("exit 0");
    assert!(exited_by(
        &mut quick,
        Instant::now() + Duration::from_secs(5)
    ));
}

#[test]
fn a_read_inside_its_deadline_returns_everything_even_when_it_is_long() {
    use std::time::Duration;
    assert_eq!(
        text_within("echo", &["hi"], Duration::from_secs(10)),
        Some("hi\n".to_owned())
    );
    // More than a pipe buffer holds: the reader must drain it or `seq` would stall.
    let long = text_within("seq", &["1", "100000"], Duration::from_secs(10)).unwrap();
    assert_eq!(long.lines().count(), 100_000);
    assert_eq!(
        text_within("/nonexistent/csr-test", &[], Duration::from_secs(1)),
        None
    );
}
