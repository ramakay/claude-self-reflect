use csr_engine::companion::{cli::PaneArgs, pane::Pane, paths::Paths};
use std::{fs, path::PathBuf, process::Command};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/companion")
}
fn home() -> (tempfile::TempDir, Paths) {
    let d = tempfile::tempdir().unwrap();
    let source = fixture().join("home");
    fn copy(from: &std::path::Path, to: &std::path::Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let e = entry.unwrap();
            if e.file_type().unwrap().is_dir() {
                copy(&e.path(), &to.join(e.file_name()));
            } else {
                fs::copy(e.path(), to.join(e.file_name())).unwrap();
            }
        }
    }
    copy(&source, d.path());
    let paths = Paths::new(d.path().into());
    // *.db is gitignored, so the DB is built from oracle.py's dump, never copied.
    let _ = fs::remove_file(&paths.db);
    let con = rusqlite::Connection::open(&paths.db).unwrap();
    con.execute_batch(include_str!("fixtures/companion/home.sql"))
        .unwrap();
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    con.execute(
        "update intent_events set transcript_path=?",
        [paths
            .projects
            .join("fixture/sess-now.jsonl")
            .to_string_lossy()
            .as_ref()],
    )
    .unwrap();
    for (id, file) in [(1, "csr-engine/src/main.rs"), (2, "csr-engine/src/lib.rs")] {
        con.execute(
            "update witness_ledger set file=?1 where id=?2",
            rusqlite::params![repo.join(file).to_string_lossy(), id],
        )
        .unwrap();
    }
    (d, paths)
}
fn args(page: &str, cols: usize, rows: usize) -> PaneArgs {
    use clap::Parser;
    #[derive(Parser)]
    struct TestArgs {
        #[command(flatten)]
        pane: PaneArgs,
    }
    TestArgs::parse_from([
        "test",
        "--repo",
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_str()
            .unwrap(),
        "--frame",
        "--page",
        page,
        "--cols",
        &cols.to_string(),
        "--rows",
        &rows.to_string(),
        "--sid",
        "sess-now",
    ])
    .pane
}
#[test]
fn frame_goldens_are_byte_equal() {
    for (cols, rows) in [(46, 30), (100, 40)] {
        for page in ["main", "log"] {
            let (_home, paths) = home();
            let mut pane = Pane::new(paths, &args(page, cols, rows));
            let actual = pane.frame(cols, rows, 1792320000.).join("\n") + "\n";
            let expected =
                fs::read_to_string(fixture().join(format!("{page}-{cols}x{rows}.txt"))).unwrap();
            assert_eq!(actual.lines().count(), expected.lines().count());
            for (i, (actual, expected)) in actual.lines().zip(expected.lines()).enumerate() {
                assert_eq!(actual, expected, "{page} {cols}x{rows}, row {}", i + 1);
            }
        }
    }
}
#[test]
fn phone_safety_at_38_columns() {
    let (_home, paths) = home();
    let mut pane = Pane::new(paths, &args("main", 38, 20));
    for (cols, rows) in [(38, 20), (23, 10), (1, 1)] {
        assert_eq!(pane.frame(cols, rows, 1792320000.).len(), rows);
    }
}
#[test]
fn cli_rejects_unported_modes_with_exit_two() {
    let (_home, paths) = home();
    let out = Command::new(env!("CARGO_BIN_EXE_csr-engine"))
        .env("HOME", paths.home)
        .args(["companion", "pane", "--repo", "/fixture", "--brief"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("not yet ported"));
}
#[test]
fn repeated_frames_report_timing_without_changing_stdout() {
    let (_home, paths) = home();
    let run = |repeat: &str| {
        Command::new(env!("CARGO_BIN_EXE_csr-engine"))
            .env("HOME", &paths.home)
            .args([
                "companion",
                "pane",
                "--repo",
                args("main", 46, 30).repo.to_str().unwrap(),
                "--frame",
                "--sid",
                "sess-now",
                "--cols",
                "46",
                "--rows",
                "30",
                "--now",
                "1792320000",
                "--repeat",
                repeat,
            ])
            .output()
            .unwrap()
    };
    let once = run("1");
    let repeated = run("3");
    assert!(once.status.success());
    assert!(repeated.status.success());
    assert_eq!(once.stdout, repeated.stdout);
    assert!(once.stderr.is_empty());
    let timing = String::from_utf8(repeated.stderr).unwrap();
    assert!(timing.contains("first frame:"));
    assert!(timing.contains("subsequent median:"));
    assert_eq!(run("0").status.code(), Some(2));
}
#[test]
fn private_json_refuses_invalid_input_and_writes_nothing() {
    use std::io::Write;
    let (_home, paths) = home();
    let target = paths.keep.join("bad.json");
    let mut p = Command::new(env!("CARGO_BIN_EXE_csr-engine"))
        .env("HOME", paths.home)
        .args([
            "companion",
            "private",
            "write",
            target.to_str().unwrap(),
            "--redact-json",
        ])
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    p.stdin.take().unwrap().write_all(b"{cut off").unwrap();
    assert_eq!(p.wait().unwrap().code(), Some(1));
    assert!(!target.exists());
}

#[test]
fn frame_mode_never_writes_to_its_home() {
    let (_home, paths) = home();
    let mut pane = Pane::new(paths.clone(), &args("main", 46, 30));
    pane.frame(46, 30, 1792320000.);
    assert!(!paths.keep.join("seen").exists());
    assert!(!paths.keep.join("previous").exists());
    assert!(!paths.keep.join("panes").exists());
}

#[test]
fn keys_and_clicks_open_rows_and_keep_a_dream_answer() {
    let (_home, paths) = home();
    let mut pane = Pane::new(paths.clone(), &args("main", 100, 40));
    pane.frame(100, 40, 1792320000.);
    let row = *pane
        .acts
        .iter()
        .find(|(_, a)| a["key"] == "dream:newest")
        .unwrap()
        .0;
    assert!(pane.click(row, 6, 1792320000.));
    assert_eq!(pane.open, "dream:newest");
    assert!(pane.key("escape"));
    assert!(pane.open.is_null());
    pane.frame(100, 40, 1792320000.);
    let row = *pane
        .acts
        .iter()
        .find(|(_, a)| a["spans"][0][2] == "helpful")
        .unwrap()
        .0;
    pane.click(row, 6, 1792320000.);
    let text = fs::read_to_string(paths.keep.join("dreams.jsonl")).unwrap();
    let answer: serde_json::Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
    assert_eq!(answer["kind"], "answer");
    assert_eq!(answer["answer"], "helpful");
    assert!(pane.key("l"));
    assert_eq!(pane.view, "log");
    assert!(pane.key("d"));
    assert_eq!(pane.view, "main");
    assert!(!pane.key("q"));
}

#[cfg(unix)]
#[test]
fn sigterm_restores_the_terminal_and_removes_the_pid_file() {
    use std::io::Read;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::time::{Duration, Instant};
    let (_home, paths) = home();
    let mut master = -1;
    let mut slave = -1;
    let mut size = libc::winsize {
        ws_row: 40,
        ws_col: 100,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let opened = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    assert_eq!(opened, 0, "openpty: {}", std::io::Error::last_os_error());
    let mut master = unsafe { fs::File::from_raw_fd(master) };
    let slave = unsafe { fs::File::from_raw_fd(slave) };
    let mut before = std::mem::MaybeUninit::<libc::termios>::uninit();
    assert_eq!(
        unsafe { libc::tcgetattr(slave.as_raw_fd(), before.as_mut_ptr()) },
        0
    );
    let before = unsafe { before.assume_init() };
    let child = Command::new(env!("CARGO_BIN_EXE_csr-engine"))
        .env("HOME", &paths.home)
        .args([
            "companion",
            "pane",
            "--repo",
            env!("CARGO_MANIFEST_DIR"),
            "--watch",
            "--sid",
            "sess-now",
        ])
        .stdin(slave.try_clone().unwrap())
        .stdout(slave.try_clone().unwrap())
        .stderr(slave.try_clone().unwrap())
        .spawn()
        .unwrap();
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    // Only this test's own child can be signalled or killed, including assertion failures.
    let mut child_guard = ChildGuard(child);
    let pid = paths.keep.join("panes/sess-now.json");
    unsafe {
        libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK);
    }
    let mut output = Vec::new();
    let start = Instant::now();
    while !pid.exists() && start.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(pid.exists(), "watch created its pid file");
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        unsafe { libc::kill(child_guard.0.id() as i32, libc::SIGTERM) },
        0
    );
    let start = Instant::now();
    while child_guard.0.try_wait().unwrap().is_none() && start.elapsed() < Duration::from_secs(10) {
        let _ = master.read_to_end(&mut output);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        child_guard.0.try_wait().unwrap().is_some(),
        "watch exited on SIGTERM"
    );
    assert!(!pid.exists());
    let mut after = std::mem::MaybeUninit::<libc::termios>::uninit();
    assert_eq!(
        unsafe { libc::tcgetattr(slave.as_raw_fd(), after.as_mut_ptr()) },
        0
    );
    let after = unsafe { after.assume_init() };
    assert_eq!(before.c_lflag, after.c_lflag);
    unsafe {
        libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK);
    }
    let _ = master.read_to_end(&mut output);
    let out = String::from_utf8_lossy(&output);
    assert!(out.contains("\x1b[?25h"));
    assert!(out.contains("\x1b]111"));
}

#[test]
fn a_stray_click_on_close_never_closes_the_pane() {
    let (_home, paths) = home();
    let mut pane = Pane::new(paths, &args("main", 46, 30));
    pane.frame(46, 30, 1792320000.);
    let span = pane.acts[&29]["keys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s[2] == "q")
        .unwrap();
    let col = span[0].as_u64().unwrap() as usize;
    assert!(pane.click(29, col, 1792320000.));
    assert!(!pane.key("q"));
}
