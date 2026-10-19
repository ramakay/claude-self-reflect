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
                let bytes = fs::read(e.path()).unwrap();
                if let Ok(text) = std::str::from_utf8(&bytes) {
                    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .parent()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned();
                    fs::write(to.join(e.file_name()), text.replace("<REPO>", &repo)).unwrap();
                } else {
                    fs::write(to.join(e.file_name()), &bytes).unwrap();
                }
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
            pane.pixels = false;
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
        .args([
            "companion",
            "pane",
            "--repo",
            "/fixture",
            "--frame",
            "--page",
            "summary",
        ])
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
    terminal_session(false);
}
#[cfg(unix)]
#[test]
fn rejected_binary_reload_keeps_the_process_heartbeat_and_pid_cleanup() {
    terminal_session(true);
}
#[cfg(unix)]
fn terminal_session(reload: bool) {
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
    let binary = paths.home.join("test-engine");
    let executable = if reload {
        fs::copy(env!("CARGO_BIN_EXE_csr-engine"), &binary).unwrap();
        binary.as_path()
    } else {
        std::path::Path::new(env!("CARGO_BIN_EXE_csr-engine"))
    };
    let child = Command::new(executable)
        .env("HOME", &paths.home)
        .env("TERM_PROGRAM", "kitty")
        .env_remove("CSR_COMPANION_PIXELS")
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
    if reload {
        let old = fs::metadata(&pid).unwrap().modified().unwrap();
        let bad = paths.home.join("bad-engine");
        fs::write(&bad, "broken binary").unwrap();
        fs::rename(&bad, &binary).unwrap();
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(7) {
            let _ = master.read_to_end(&mut output);
            assert!(
                child_guard.0.try_wait().unwrap().is_none(),
                "the old process must survive"
            );
            if fs::metadata(&pid).unwrap().modified().unwrap() > old
                && String::from_utf8_lossy(&output).contains("engine reload:")
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(String::from_utf8_lossy(&output).contains("engine reload:"));
        assert!(fs::metadata(&pid).unwrap().modified().unwrap() > old);
    }
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
    assert!(
        out.contains("a=T,f=100,q=2,C=1"),
        "watch sends its PNG pictures"
    );
    assert!(out.contains(csr_engine::companion::render::kitty::DELETE_ALL));
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

#[test]
fn engine_probe_identifies_the_companion_subcommand() {
    let (_home, paths) = home();
    let out = Command::new(env!("CARGO_BIN_EXE_csr-engine"))
        .env("HOME", paths.home)
        .args(["companion", "--version"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!("csr-engine-companion {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn pixel_frames_are_byte_equal_to_pillow_layouts() {
    for (cols, rows) in [(46, 30), (100, 40)] {
        for page in ["main", "log"] {
            let (_home, paths) = home();
            let mut pane = Pane::new(paths, &args(page, cols, rows));
            pane.pixels = true;
            let actual = pane.frame(cols, rows, 1792320000.).join("\n") + "\n";
            let expected =
                fs::read_to_string(fixture().join(format!("pixels-{page}-{cols}x{rows}.txt")))
                    .unwrap();
            for (i, (a, b)) in actual.lines().zip(expected.lines()).enumerate() {
                assert_eq!(a, b, "pixels {page} {cols}x{rows}, row {}", i + 1);
            }
            assert_eq!(actual, expected);
            assert!(!pane.placed.is_empty());
            assert!(pane.placed.iter().all(|p| p.row + p.rows < rows));
            if page == "main" {
                assert!(pane.placed.iter().any(|p| p.cols == 4 && p.rows == 1));
            }
            eprintln!("text parity: {page} {cols}x{rows}: {rows} rows byte equal");
        }
    }
}

fn assert_picture(name: &str, pixmap: tiny_skia::Pixmap) {
    use csr_engine::companion::render::raster;
    let expected = image::open(fixture().join("pictures").join(format!("{name}.png")))
        .unwrap()
        .to_rgba8();
    let actual = image::load_from_memory(&raster::png(&pixmap).unwrap())
        .unwrap()
        .to_rgba8();
    assert_eq!(
        actual.dimensions(),
        expected.dimensions(),
        "{name} dimensions"
    );
    let difference: f64 = actual
        .as_raw()
        .iter()
        .zip(expected.as_raw())
        .map(|(&a, &b)| (f64::from(a) - f64::from(b)).abs())
        .sum::<f64>()
        / actual.as_raw().len() as f64;
    // 4/255 allows 1x AA versus Pillow's 2x Lanczos edges, and font hinting,
    // while rejecting missing shapes, wrong grounds and misplaced chart data.
    let threshold = 4.;
    eprintln!("pixel difference: {name}: {difference:.4}, threshold {threshold}");
    assert!(
        difference < threshold,
        "{name}: {difference:.4} >= {threshold}"
    );
}
#[test]
fn pictures_match_pillow_dimensions_and_pixels() {
    use csr_engine::companion::render::{raster::*, text};
    let facts: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/companion/pictures.json")).unwrap();
    let cell = (16, 34);
    let backs = text::dark_steps(3);
    let ground = text::tint("last", 0.07);
    let fonts = csr_engine::companion::sources::picture_fonts();
    let f = SessionFacts::from_value(&facts["session"], 2030.);
    assert_picture("session-rich", session(&f, 40, cell, &backs).unwrap());
    assert_picture("session-light", session(&f, 40, cell, &[]).unwrap());
    assert_picture(
        "last-rich",
        last(&LastFacts::from_value(&facts["last"]), 43, 3, cell, ground).unwrap(),
    );
    // These three draw text. Their goldens are in the Mac's Menlo, as pane.py draws
    // them; where Menlo is missing (Linux CI) the text comes from another font.
    let menlo = std::path::Path::new("/System/Library/Fonts/Menlo.ttc").exists();
    if !menlo {
        eprintln!("pixel difference: payback, paid, context: skipped, no Menlo");
    }
    for name in ["payback", "paid"].into_iter().filter(|_| menlo) {
        assert_picture(
            name,
            payback(
                &PaybackFacts::from_value(&facts[name]),
                40,
                5,
                cell,
                ground,
                fonts,
            )
            .unwrap(),
        );
    }
    if menlo {
        assert_picture(
            "context",
            context(
                &ContextFacts::from_value(&facts["context"]),
                40,
                3,
                cell,
                &backs,
                fonts,
            )
            .unwrap(),
        );
    }
    for name in [
        "session",
        "touching",
        "risk",
        "rules",
        "dreams",
        "memory",
        "changed",
        "remembered",
        "kept",
        "story",
        "coach",
        "told",
        "last",
        "todos",
        "compacted",
        "flight",
        "earlier",
        "lost",
    ] {
        assert_picture(
            &format!("iso-{name}"),
            iso(name, cell, [text::tint(name, 0.10), text::tint(name, 0.07)]).unwrap(),
        );
    }
    assert_picture("emblem", emblem(cell, [backs[0], backs[1]]).unwrap());
    assert_picture("logo", logo(cell, backs[0]).unwrap());
}

#[test]
fn png_mode_writes_the_oracle_session_and_screen_without_touching_home() {
    let (_home, paths) = home();
    let output = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_csr-engine"))
        .env("HOME", &paths.home)
        .env("TERM_PROGRAM", "kitty")
        .env_remove("CSR_COMPANION_PIXELS")
        .env("CSR_COMPANION_NOW", "1792320000")
        .args([
            "companion",
            "pane",
            "--repo",
            args("main", 46, 30).repo.to_str().unwrap(),
            "--sid",
            "sess-now",
            "--png",
            output.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!("{}\n", output.path().display())
    );
    let actual = fs::read_to_string(output.path().join("screen.txt")).unwrap();
    let expected = fs::read_to_string(fixture().join("pictures/screen.txt")).unwrap();
    assert_eq!(actual, expected);
    let actual = image::open(output.path().join("session.png"))
        .unwrap()
        .to_rgba8();
    let expected = image::open(fixture().join("pictures/session.png"))
        .unwrap()
        .to_rgba8();
    assert_eq!(actual.dimensions(), expected.dimensions());
    let difference = actual
        .as_raw()
        .iter()
        .zip(expected.as_raw())
        .map(|(&a, &b)| (f64::from(a) - f64::from(b)).abs())
        .sum::<f64>()
        / actual.as_raw().len() as f64;
    eprintln!("pixel difference: session-export: {difference:.4}, threshold 4");
    assert!(difference < 4.);
    assert!(!paths.keep.join("seen").exists());
    assert!(!paths.keep.join("previous").exists());
    assert!(!paths.keep.join("panes").exists());
    assert!(!output.path().join("last.png").exists());
}

#[test]
fn disabling_pixels_keeps_text_strips_even_in_kitty() {
    let (_home, paths) = home();
    let out = Command::new(env!("CARGO_BIN_EXE_csr-engine"))
        .env("HOME", paths.home)
        .env("TERM_PROGRAM", "kitty")
        .env("CSR_COMPANION_PIXELS", "0")
        .args([
            "companion",
            "pane",
            "--repo",
            args("main", 46, 30).repo.to_str().unwrap(),
            "--sid",
            "sess-now",
            "--frame",
            "--cols",
            "46",
            "--rows",
            "30",
            "--now",
            "1792320000",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        out.stdout,
        fs::read(fixture().join("main-46x30.txt")).unwrap()
    );
}

#[cfg(test)]
mod lease_tests {
    use csr_engine::companion::lease::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn args(step: Step, root: &str, rank: i64, now: f64, busy: bool) -> LeaseArgs {
        LeaseArgs {
            step,
            folder: PathBuf::new(),
            session: "s".into(),
            root: root.into(),
            rank,
            now,
            busy,
        }
    }

    #[test]
    fn test_a_fresh_lease_another_copy_holds_is_kept_from_equal_or_lower_rank_and_from_anyone_while_busy(
    ) {
        let now = 1_760_000_000_000.;
        let mut held = json!({"root":"installed","rank":0,"at":now});
        assert!(decide(&args(Step::Take, "installed", 0, now + 1., false), &held).0);
        assert!(decide(&args(Step::Take, "dev", 1, now + 1., false), &held).0);
        held["rank"] = 1.into();
        assert_eq!(
            decide(&args(Step::Take, "dev", 1, now + 1., false), &held),
            (false, None)
        );
        held["rank"] = 0.into();
        held["busy"] = true.into();
        assert!(!decide(&args(Step::Take, "dev", 1, now + 1., false), &held).0);
        held["at"] = (now - 900_000. - 1.).into();
        assert!(decide(&args(Step::Take, "dev", 1, now, true), &held).0);
        assert!(decide(&args(Step::Take, "dev", 1, now, false), &json!("garbled")).0);
    }

    #[test]
    fn test_release_keeps_the_lease_and_drops_busy_renew_keeps_busy_neither_touches_another_copys()
    {
        let held = json!({"root":"dev","rank":1,"at":1000,"busy":true});
        let release = decide(&args(Step::Release, "dev", 1, 1005., false), &held);
        assert_eq!(
            release,
            (true, Some(json!({"root":"dev","rank":1,"at":1000})))
        );
        let renew = decide(&args(Step::Renew, "dev", 1, 1005., false), &held);
        assert_eq!(renew.1.unwrap()["busy"], true);
        for step in [Step::Renew, Step::Release] {
            assert_eq!(
                decide(&args(step, "other", 1, 1005., false), &held),
                (false, None)
            );
        }
    }
}

#[cfg(test)]
mod lease_process_tests {
    use super::*;
    use csr_engine::companion::lease::{decide, LeaseArgs, LeaseOutput, Step};
    use serde_json::Value;
    use std::process::{Child, Stdio};
    use std::time::{Duration, Instant};

    fn start(folder: &std::path::Path, root: &str, pause: &str) -> Child {
        Command::new(env!("CARGO_BIN_EXE_csr-engine"))
            .env("HOME", folder)
            .env("CSR_LEASE_PAUSE_MS", pause)
            .args(["companion", "lease", "take", "--folder"])
            .arg(folder)
            .args([
                "--session",
                "s",
                "--root",
                root,
                "--rank",
                "1",
                "--now",
                "1760000000000",
                "--busy",
            ])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap()
    }

    #[test]
    fn lease_oracle_is_parse_equal() {
        let rows: Vec<Value> =
            serde_json::from_str(include_str!("fixtures/companion/lease.json")).unwrap();
        for row in rows {
            let args = LeaseArgs {
                step: match row["step"].as_str().unwrap() {
                    "take" => Step::Take,
                    "renew" => Step::Renew,
                    _ => Step::Release,
                },
                folder: PathBuf::new(),
                session: "s".into(),
                root: row["root"].as_str().unwrap().into(),
                rank: row["rank"].as_i64().unwrap(),
                now: 1792320000001.,
                busy: row["busy"].as_bool().unwrap(),
            };
            let (held, lease) = decide(&args, &row["held"]);
            let output = LeaseOutput {
                held,
                lease: lease.unwrap_or(row["held"].clone()),
            };
            assert_eq!(serde_json::to_value(output).unwrap(), row["output"]);
        }
    }

    #[test]
    fn test_of_n_copies_that_ask_at_once_exactly_one_holds_the_lease() {
        for n in [2, 3, 8] {
            let folder = tempfile::tempdir().unwrap();
            let runs: Vec<_> = (0..n)
                .map(|i| start(folder.path(), &format!("/copy/{i}"), "150"))
                .collect();
            let answers: Vec<LeaseOutput> = runs
                .into_iter()
                .map(|run| {
                    let out = run.wait_with_output().unwrap();
                    assert!(out.status.success());
                    serde_json::from_slice(&out.stdout).unwrap()
                })
                .collect();
            assert_eq!(answers.iter().filter(|a| a.held).count(), 1);
            let kept: Value =
                serde_json::from_slice(&fs::read(folder.path().join("s.json")).unwrap()).unwrap();
            assert_eq!(kept, answers.iter().find(|a| a.held).unwrap().lease);
        }
    }

    #[test]
    fn test_a_copy_killed_holding_the_lock_leaves_nothing_to_reclaim() {
        use fs2::FileExt;
        let folder = tempfile::tempdir().unwrap();
        let mut dead = start(folder.path(), "/copy/dead", "5000");
        let path = folder.path().join("s.lease.lock");
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(file) = fs::OpenOptions::new().write(true).open(&path) {
                if file.try_lock_exclusive().is_err() {
                    break;
                }
            }
            assert!(Instant::now() < until, "child never acquired its lock");
            std::thread::sleep(Duration::from_millis(10));
        }
        dead.kill().unwrap();
        dead.wait().unwrap();
        let began = Instant::now();
        let out = start(folder.path(), "/copy/alive", "0")
            .wait_with_output()
            .unwrap();
        assert!(
            serde_json::from_slice::<LeaseOutput>(&out.stdout)
                .unwrap()
                .held
        );
        assert!(began.elapsed() < Duration::from_secs(5));
        assert!(path.exists());
    }
}

fn normalized(mut value: serde_json::Value, paths: &Paths) -> serde_json::Value {
    fn visit(v: &mut serde_json::Value, replacements: &[(&str, &str)]) {
        match v {
            serde_json::Value::Number(n) => *v = serde_json::Value::from(n.as_f64().unwrap()),
            serde_json::Value::String(s) => {
                for (from, to) in replacements {
                    *s = s.replace(from, to);
                }
            }
            serde_json::Value::Array(a) => a.iter_mut().for_each(|v| visit(v, replacements)),
            serde_json::Value::Object(o) => o.values_mut().for_each(|v| visit(v, replacements)),
            _ => (),
        }
    }
    let repo = args("main", 46, 30).repo;
    visit(
        &mut value,
        &[
            (paths.home.to_str().unwrap(), "<HOME>"),
            (repo.to_str().unwrap(), "<REPO>"),
        ],
    );
    value
}

fn hook(paths: &Paths, argv: &[&str], stdin: &str) -> std::process::Output {
    use std::io::Write;
    let mut child = Command::new(env!("CARGO_BIN_EXE_csr-engine"))
        .env("HOME", &paths.home)
        .env("CSR_COMPANION_NOW", "1792320000")
        .args(["companion", "pane", "--repo"])
        .arg(args("main", 46, 30).repo)
        .args(argv)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn json_and_note_hook_goldens_are_parse_equal() {
    for (name, argv, stdin) in [
        (
            "json",
            vec!["--json", "--limit", "60", "--sid", "sess-now"],
            "",
        ),
        (
            "note",
            vec![
                "--note",
                concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.rs"),
                "--touch",
                "-",
            ],
            r#"{"tool":"Read","offset":1,"limit":20}"#,
        ),
    ] {
        let (_home, paths) = home();
        let out = hook(&paths, &argv, stdin);
        assert!(
            out.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let actual = normalized(serde_json::from_slice(&out.stdout).unwrap(), &paths);
        let expected = normalized(
            serde_json::from_slice(&fs::read(fixture().join(format!("{name}.json"))).unwrap())
                .unwrap(),
            &paths,
        );
        assert_eq!(actual, expected, "{name}");
    }
}

#[test]
fn test_the_dream_log_takes_kind_compact_with_its_repo() {
    use std::os::unix::fs::PermissionsExt;
    let (_home, paths) = home();
    let out = hook(
        &paths,
        &[
            "--keep",
            r#"{"kind":"compact","session":"sess-now","at":1792320000,"event":"plan","token":"abc"}"#,
        ],
        "",
    );
    assert!(out.status.success());
    assert_eq!(out.stdout, b"{}\n");
    let text = fs::read_to_string(paths.keep.join("dreams.jsonl")).unwrap();
    let actual = normalized(
        serde_json::from_str(text.lines().last().unwrap()).unwrap(),
        &paths,
    );
    let expected = normalized(
        serde_json::from_str(include_str!("fixtures/companion/keep-record.json")).unwrap(),
        &paths,
    );
    assert_eq!(actual, expected);
    assert_eq!(
        fs::metadata(paths.keep.join("dreams.jsonl"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn lost_and_compact_hook_goldens_are_parse_equal() {
    for (name, argv) in [
        ("lost", vec!["--lost", "--sid", "sess-lost"]),
        (
            "compact-plan",
            vec!["--compact-plan", "--sid", "sess-now", "--ctx", "300000"],
        ),
        ("compact-usual", vec!["--compact-usual"]),
    ] {
        let (_home, paths) = home();
        let out = hook(&paths, &argv, "");
        assert!(
            out.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            normalized(serde_json::from_slice(&out.stdout).unwrap(), &paths),
            normalized(
                serde_json::from_slice(&fs::read(fixture().join(format!("{name}.json"))).unwrap())
                    .unwrap(),
                &paths
            ),
            "{name}"
        );
    }
}

#[test]
fn brief_is_byte_equal_and_coach_and_story_news_are_parse_equal() {
    let (_home, paths) = home();
    let out = hook(&paths, &["--brief", "--sid", "sess-now"], "");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let actual = normalized(
        serde_json::Value::String(String::from_utf8(out.stdout).unwrap()),
        &paths,
    );
    assert_eq!(
        actual.as_str().unwrap(),
        include_str!("fixtures/companion/brief.txt")
    );
    for (name, mode) in [("coach", "--coach"), ("story-news", "--story-news")] {
        let out = hook(&paths, &[mode, "--sid", "sess-now"], "");
        assert!(
            out.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            normalized(serde_json::from_slice(&out.stdout).unwrap(), &paths),
            normalized(
                serde_json::from_slice(&fs::read(fixture().join(format!("{name}.json"))).unwrap())
                    .unwrap(),
                &paths
            ),
            "{name}"
        );
    }
}

#[cfg(test)]
#[path = "support/companion_python_cases.rs"]
mod python_cases;

#[cfg(test)]
mod continued_tests {
    use super::*;
    use csr_engine::companion::{session, sources};
    fn transcripts() -> (tempfile::TempDir, Paths, PathBuf, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().into());
        let folder = paths.projects.join("p");
        fs::create_dir_all(&folder).unwrap();
        let old = folder.join("old.jsonl");
        let new = folder.join("new.jsonl");
        fs::write(
            &old,
            concat!("{\"type\":\"user\",\"entrypoint\":\"cli\"}\n",
            "{\"type\":\"continued-in\",\"sessionId\":\"old\",\"continuedInSessionId\":\"new\"}\n",
            "{\"type\":\"last-prompt\",\"sessionId\":\"old\"}\n"),
        )
        .unwrap();
        fs::write(&new, "{}\n").unwrap();
        (d, paths, old, new)
    }
    #[test]
    fn test_continued_from_finds_the_session_this_one_continues() {
        let (_d, paths, _, _) = transcripts();
        assert_eq!(
            sources::continued_from_at(&paths, "new", None, 1000.).as_deref(),
            Some("old")
        );
        assert!(sources::continued_from_at(&paths, "old", None, 1000.).is_none());
        assert_eq!(session::lineage(&paths, "new"), ["new", "old"]);
    }
    #[test]
    fn test_a_row_naming_another_session_or_an_odd_id_is_not_followed() {
        for row in [
            "{\"type\":\"continued-in\",\"sessionId\":\"old\",\"continuedInSessionId\":\"other\"}\n",
            "{\"type\":\"continued-in\",\"sessionId\":\"../x y\",\"continuedInSessionId\":\"new\"}\n",
        ] {
            let (_d,paths,old,_) = transcripts(); fs::write(old,row).unwrap();
            assert!(sources::continued_from_at(&paths,"new",None,1000.).is_none());
        }
    }
    #[test]
    fn test_none_is_looked_for_again_after_a_minute_and_a_find_is_kept() {
        let (_d, paths, old, _) = transcripts();
        let rows = fs::read(&old).unwrap();
        fs::write(&old, "").unwrap();
        assert!(sources::continued_from_at(&paths, "new", None, 1000.).is_none());
        fs::write(&old, rows).unwrap();
        assert!(sources::continued_from_at(&paths, "new", None, 1059.).is_none());
        assert_eq!(
            sources::continued_from_at(&paths, "new", None, 1061.).as_deref(),
            Some("old")
        );
        fs::remove_file(old).unwrap();
        assert_eq!(
            sources::continued_from_at(&paths, "new", None, 4661.).as_deref(),
            Some("old")
        );
    }
    #[test]
    fn test_a_row_speaks_only_for_the_file_it_is_in() {
        let (_d, paths, old, new) = transcripts();
        let stranger = new.parent().unwrap().join("stranger.jsonl");
        fs::write(
            &stranger,
            concat!(
                "{\"type\":\"continued-in\",\"sessionId\":\"forged\",",
                "\"continuedInSessionId\":\"new\"}\n"
            ),
        )
        .unwrap();
        fs::remove_file(old).unwrap();
        assert!(sources::continued_from_at(&paths, "new", None, 1000.).is_none());
    }
    #[test]
    fn test_a_miss_is_not_kept_while_the_own_transcript_is_missing() {
        let (_d, paths, old, new) = transcripts();
        let parked = new.with_extension("parked");
        fs::rename(&new, &parked).unwrap();
        assert!(sources::continued_from_at(&paths, "new", None, 1000.).is_none());
        fs::rename(parked, &new).unwrap();
        assert!(old.exists());
        assert_eq!(
            sources::continued_from_at(&paths, "new", None, 1000.).as_deref(),
            Some("old")
        );
    }
    #[test]
    fn test_an_explicit_path_that_is_not_there_yet_is_not_a_scan_and_is_not_kept() {
        let (_d, paths, _, new) = transcripts();
        let parked = new.with_extension("parked");
        fs::rename(&new, &parked).unwrap();
        assert!(sources::continued_from_at(&paths, "new", Some(&new), 1000.).is_none());
        assert!(sources::previous_session(&paths, &new, 4000.).is_none());
        fs::rename(parked, &new).unwrap();
        assert_eq!(
            sources::continued_from_at(&paths, "new", Some(&new), 1000.).as_deref(),
            Some("old")
        );
    }
    #[test]
    fn test_a_transcript_that_vanishes_during_the_scan_is_not_kept_as_a_miss() {
        let (_d, paths, _, new) = transcripts();
        let parked = paths.home.join("parked");
        fs::rename(new.parent().unwrap(), &parked).unwrap();
        assert!(sources::continued_from_at(&paths, "new", Some(&new), 1000.).is_none());
        fs::rename(parked, new.parent().unwrap()).unwrap();
        assert_eq!(
            sources::continued_from_at(&paths, "new", Some(&new), 1000.).as_deref(),
            Some("old")
        );
    }
    #[test]
    fn test_a_cycle_ends_the_lineage() {
        let (_d, paths, old, new) = transcripts();
        fs::write(
            new,
            concat!(
                "{\"type\":\"continued-in\",\"sessionId\":\"new\",",
                "\"continuedInSessionId\":\"old\"}\n"
            ),
        )
        .unwrap();
        assert!(old.exists());
        assert_eq!(session::lineage(&paths, "new"), ["new", "old"]);
    }
    #[test]
    fn test_previous_session_returns_the_continued_from_file_though_it_was_written_after_this_began(
    ) {
        let (_d, paths, old, new) = transcripts();
        assert_eq!(sources::previous_session(&paths, &new, 4000.), Some(old));
    }
    #[test]
    fn test_the_kill_switch_returns_none() {
        let (_d, paths, _, _) = transcripts();
        fs::create_dir_all(paths.db.parent().unwrap()).unwrap();
        let con = rusqlite::Connection::open(&paths.db).unwrap();
        con.execute_batch(include_str!("fixtures/companion/home.sql"))
            .unwrap();
        for id in ["old", "old", "old", "new"] {
            con.execute("insert into retrieval_events(session_id) values(?)", [id])
                .unwrap();
        }
        drop(con);
        let out = Command::new(env!("CARGO_BIN_EXE_csr-engine"))
            .env("HOME", &paths.home)
            .env("CSR_NO_CONTINUED", "1")
            .args([
                "companion",
                "pane",
                "--repo",
                args("main", 46, 30).repo.to_str().unwrap(),
                "--json",
                "--sid",
                "new",
            ])
            .output()
            .unwrap();
        assert!(out.status.success());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["recalled"],
            1
        );
    }
}

#[cfg(test)]
mod continued_memory_tests {
    use super::*;
    use csr_engine::companion::{coach, db::Db, hook_json, pages, recall, session, story};
    use serde_json::{json, Value};
    fn fixture_memory() -> (tempfile::TempDir, Paths, rusqlite::Connection) {
        let (d, paths) = home();
        let folder = paths.projects.join("continued");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("new.jsonl"), "{}\n").unwrap();
        fs::write(
            folder.join("old.jsonl"),
            concat!(
                "{\"type\":\"continued-in\",",
                "\"sessionId\":\"old\",\"continuedInSessionId\":\"new\"}\n"
            ),
        )
        .unwrap();
        let con = rusqlite::Connection::open(&paths.db).unwrap();
        (d, paths, con)
    }
    fn tell(con: &rusqlite::Connection, sid: &str, rev: usize) {
        con.execute(
            "insert into reflections values(?1,?2,?3,'2026-10-15T01:00:00Z')",
            rusqlite::params![
                format!("story_{sid}"),
                format!("Story of {sid}. Where it stands: done."),
                json!([
                    "session_story",
                    format!("conv_{sid}"),
                    format!("story_rev_{rev}")
                ])
                .to_string()
            ],
        )
        .unwrap();
    }
    fn map(con: &rusqlite::Connection, sid: &str) {
        con.execute(
            "insert into meta values(?1,?2)",
            rusqlite::params![
                format!("story_map:{sid}"),
                json!({"v":1,"current":{"rev":3,"at":1000,"by":"Haiku","covered_msgs":5,
                "entries":[{"kind":"shift","claim":"Added a thing","before":[],
                "after":[{"scope":"a.py","sym":"f","mark":"+","tier":"verified"}],
                "receipts":["oid:abcdef0"],"grounded":true}]}})
                .to_string()
            ],
        )
        .unwrap();
    }
    #[test]
    fn test_session_story_own_wins_and_the_fallback_names_where_it_came_from() {
        let (_d, paths, con) = fixture_memory();
        tell(&con, "old", 39);
        let db = Db::open(&paths).unwrap();
        let old = recall::session_story(&db, &paths, "new");
        assert_eq!(
            (old["rev"].clone(), old["from_sid"].clone()),
            (json!(39), json!("old"))
        );
        assert!(recall::session_story(&db, &paths, "old")
            .get("from_sid")
            .is_none());
        tell(&con, "new", 1);
        let own = recall::session_story(&db, &paths, "new");
        assert_eq!(own["rev"], 1);
        assert!(own.get("from_sid").is_none());
    }
    #[test]
    fn test_the_story_heading_says_where_an_inherited_story_is_from() {
        let (_d, paths, con) = fixture_memory();
        tell(&con, "old", 39);
        let db = Db::open(&paths).unwrap();
        for (sid, inherited) in [("new", true), ("old", false)] {
            let story = recall::session_story(&db, &paths, sid);
            let data = json!({"repo":"/fixture","story":story,"dreams":[],"dreams_on":false});
            let s = json!({"id":sid,"turn_end":1});
            let page = pages::page_main(&data, &s, 0., &Value::Null, None);
            let right = page["sections"][0]["right"].as_str().unwrap();
            assert_eq!(right.contains("from old"), inherited);
        }
    }
    #[test]
    fn test_display_story_map_prefers_its_own_map_then_the_predecessors() {
        let (_d, paths, con) = fixture_memory();
        let db = Db::open(&paths).unwrap();
        assert!(story::display_story_map(&db, &paths, "new").is_none());
        map(&con, "old");
        assert_eq!(
            story::display_story_map(&db, &paths, "new").unwrap().1,
            "old"
        );
        map(&con, "new");
        assert_eq!(
            story::display_story_map(&db, &paths, "new").unwrap().1,
            "new"
        );
    }
    #[test]
    fn test_the_map_title_carries_from_only_when_inherited() {
        let (_d, paths, con) = fixture_memory();
        map(&con, "old");
        let db = Db::open(&paths).unwrap();
        assert!(hook_json::shift_map_json(&db, &paths, "new")
            .unwrap()
            .title
            .ends_with(" · from old"));
        assert!(!hook_json::shift_map_json(&db, &paths, "old")
            .unwrap()
            .title
            .contains("from"));
    }
    #[test]
    fn test_recalled_sums_the_lineage() {
        let (_d, paths, con) = fixture_memory();
        for (id, n) in [("old", 174), ("new", 2), ("someone-else", 9)] {
            for _ in 0..n {
                con.execute("insert into retrieval_events(session_id) values(?)", [id])
                    .unwrap();
            }
        }
        assert_eq!(session::lineage(&paths, "new"), ["new", "old"]);
        for (id, n) in [("new", 176), ("old", 174), ("", 0)] {
            assert_eq!(
                hook_json::pane_json(&paths, &args("main", 46, 30).repo, id, 10, 0.).recalled,
                n
            );
        }
    }
    #[test]
    fn test_story_news_stays_on_the_own_session() {
        let (_d, paths, con) = fixture_memory();
        map(&con, "old");
        assert_eq!(
            serde_json::to_value(story::story_news(&paths, None, "new", 0.)).unwrap(),
            json!({"story":false})
        );
    }
    #[test]
    fn test_how_claude_read_falls_back_to_the_predecessor_that_has_readings() {
        let (_d, paths, _con) = fixture_memory();
        let old = paths.projects.join("continued/old.jsonl");
        let continued = fs::read_to_string(&old).unwrap();
        fs::write(
            old,
            json!({"type":"user","isCompactSummary":true,
            "timestamp":"2026-10-15T01:10:00Z","message":{
                "content":"Interpreted as: \"dknr\" meant \"done\"."}})
            .to_string()
                + "\n"
                + &continued,
        )
        .unwrap();
        let found = coach::how_claude_read(&paths, "new", 2);
        assert!(!found.is_empty());
        assert_eq!(
            serde_json::to_value(found).unwrap(),
            serde_json::to_value(coach::how_claude_read(&paths, "old", 2)).unwrap()
        );
        assert!(coach::how_claude_read(&paths, "not-a-session", 2).is_empty());
    }
}

#[test]
fn test_old_failure_does_not_reopen_retry() {
    use csr_engine::companion::agents;
    use serde_json::json;
    let d = tempfile::tempdir().unwrap();
    let state = d.path().join("s.json");
    let drafts = d.path().join("s.drafts.jsonl");
    let row = |request: &str, outcome: &str, at: f64| {
        json!({"request":request,"session":"s",
        "thread":"0123456789ab","drafted_at":at-10.,"submitted_at":at*1000.,
        "spawn_tool_use_id":"u1","agent_id":"a1","turn_id":"t1","outcome":outcome,
        "ended_at":(at+10.)*1000.,"reason":null})
    };
    fs::write(
        &state,
        json!({"version":1,"session":"s","handoffs":[
        row("req-1","failed",1000.),row("req-2","succeeded",1100.)]})
        .to_string(),
    )
    .unwrap();
    // A final draft without LF is read too; an existing request is not duplicated.
    fs::write(
        &drafts,
        concat!(
            "{\"request\":\"req-1\",\"thread\":\"0123456789ab\",\"drafted_at\":990}\n",
            "not json\n{\"request\":\"req-3\",\"thread\":\"0123456789ab\",\"drafted_at\":1200}"
        ),
    )
    .unwrap();
    let found = agents::read_handoffs(&state, &drafts, "s");
    assert_eq!(found.len(), 3);
    assert_eq!(agents::decide(&found).unwrap().0, "handed");
    assert_eq!(agents::decide(&found).unwrap().1.request, "req-2");
    assert_eq!(found[2].outcome, "draft");
    assert!(agents::read_handoffs(&state, &drafts, "../x").is_empty());
}

/// `csr-engine companion statusline`, run for real under a throwaway HOME with a pane "open"
/// (this process's pid), as the Node wrapper's pytest suites ran statusline.cjs.
mod statusline_tests {
    use serde_json::{json, Value};
    use std::io::Write;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output, Stdio};
    use std::time::{Duration, SystemTime};
    use std::{fs, thread};

    const SH: &str = "cat >/dev/null; echo drawn";

    fn run(home: &Path, args: &[&str], input: &[u8], env: &[(&str, &str)]) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_csr-engine"))
            .args(["companion", "statusline"])
            .args(args)
            .env("HOME", home)
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let input = input.to_vec();
        let feeder = thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
        let out = child.wait_with_output().unwrap();
        feeder.join().unwrap();
        out
    }
    fn text(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).into_owned()
    }
    /// What a run printed and exited with: stdout, stderr, status code.
    fn seen(out: &Output) -> (String, String, Option<i32>) {
        (text(&out.stdout), text(&out.stderr), out.status.code())
    }
    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }
    fn keep(home: &Path, name: &str) -> PathBuf {
        home.join(".claude-self-reflect").join(name)
    }
    /// A pane "open" for `session`: its pid file names this test process.
    fn open_pane(home: &Path, name: &str, session: &str) -> PathBuf {
        let folder = keep(home, name);
        fs::create_dir_all(folder.join("panes")).unwrap();
        let body = json!({ "pid": std::process::id() }).to_string();
        fs::write(folder.join("panes").join(format!("{session}.json")), body).unwrap();
        folder
    }
    fn kept(folder: &Path, session: &str) -> Value {
        let file = folder.join("statusline").join(format!("{session}.json"));
        serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap()
    }
    fn status() -> Value {
        json!({
            "session_id": "sess-w", "model": {"display_name": "Opus"},
            "prompt_cache": {"warm": true, "ttl": "1h", "expires_at": 1800003000,
                "hit_ratio": 0.91, "misses": 0, "last_miss_cause": null,
                "recache_tokens_if_cold": 152000},
            "rate_limits": {"five_hour": {"used_percentage": 34, "resets_at": 1800003600},
                "seven_day": {"used_percentage": 12, "resets_at": 1800400000}},
            "context_window": {"total_input_tokens": 57000, "context_window_size": 1000000,
                "remaining_percentage": 94},
        })
    }
    fn input(value: &Value) -> Vec<u8> {
        value.to_string().into_bytes()
    }
    fn strings(a: &str, b: &str, code: i32) -> (String, String, Option<i32>) {
        (a.into(), b.into(), Some(code))
    }

    #[test]
    fn test_the_wrapped_commands_stderr_passes_through_and_a_line_exits_0() {
        let home = tempfile::tempdir().unwrap();
        let data = input(&json!({"session_id": "sess-1"}));
        let out = run(
            home.path(),
            &["--sh", "echo line; echo oops >&2; exit 3"],
            &data,
            &[],
        );
        assert_eq!(seen(&out), strings("line\n", "oops\n", 0));
        // Nothing to show: its own status says it failed.
        let out = run(home.path(), &["--sh", "echo oops >&2; exit 3"], &data, &[]);
        assert_eq!(seen(&out), strings("", "oops\n", 3));
    }

    #[test]
    fn test_a_command_that_never_reads_stdin_adds_no_error() {
        let home = tempfile::tempdir().unwrap();
        let big = json!({"session_id": "sess-1", "pad": "x".repeat(4_000_000)});
        let out = run(home.path(), &["--sh", "echo line"], &input(&big), &[]);
        assert_eq!(seen(&out), strings("line\n", "", 0));
    }

    #[test]
    fn test_the_argv_form_runs_no_shell_and_the_commands_own_flags_pass_through() {
        let home = tempfile::tempdir().unwrap();
        let data = input(&json!({"session_id": "sess-1"}));
        let out = run(home.path(), &["sh", "-c", "echo fine"], &data, &[]);
        assert_eq!(seen(&out), strings("fine\n", "", 0));
        let out = run(
            home.path(),
            &["/bin/echo", "-n", "--help", "--version"],
            &data,
            &[],
        );
        assert_eq!(seen(&out), strings("--help --version", "", 0));
        // No shell: a metacharacter is just text.
        let out = run(home.path(), &["/bin/echo", "a;", "echo", "b"], &data, &[]);
        assert_eq!(text(&out.stdout), "a; echo b\n");
    }

    #[test]
    fn test_several_words_after_sh_are_joined_with_one_space() {
        let home = tempfile::tempdir().unwrap();
        let out = run(
            home.path(),
            &["--sh", "echo", "a", "&&", "echo", "b"],
            b"{}",
            &[],
        );
        assert_eq!(text(&out.stdout), "a\nb\n");
        // A word that looks like a flag is still one of the shell string's words.
        let out = run(
            home.path(),
            &["--sh", "printf", "%s.", "-n", "x"],
            b"{}",
            &[],
        );
        assert_eq!(text(&out.stdout), "-n.x.");
    }

    #[test]
    fn test_a_command_that_cannot_start_says_why_on_stderr() {
        let home = tempfile::tempdir().unwrap();
        let out = run(
            home.path(),
            &["/nonexistent/csr-statusline-command"],
            b"{}",
            &[],
        );
        assert_eq!((text(&out.stdout), out.status.code()), ("".into(), Some(1)));
        let want = "statusline: spawnSync /nonexistent/csr-statusline-command ENOENT";
        assert!(text(&out.stderr).contains(want), "{}", text(&out.stderr));
    }

    #[test]
    fn test_no_command_prints_nothing_and_exits_0() {
        let home = tempfile::tempdir().unwrap();
        for args in [vec![], vec!["--sh"], vec!["--sh", ""]] {
            let out = run(home.path(), &args, &input(&status()), &[]);
            assert_eq!(seen(&out), strings("", "", 0));
        }
    }

    #[test]
    fn test_no_command_with_a_pane_open_keeps_an_empty_line() {
        let home = tempfile::tempdir().unwrap();
        let folder = open_pane(home.path(), "csr-companion", "sess-w");
        let out = run(home.path(), &[], &input(&status()), &[]);
        assert_eq!(seen(&out), strings("", "", 0));
        assert_eq!(kept(&folder, "sess-w")["line"], "");
    }

    #[test]
    fn test_a_command_that_runs_out_of_time_is_killed_after_5_seconds() {
        let home = tempfile::tempdir().unwrap();
        let start = std::time::Instant::now();
        let out = run(
            home.path(),
            &["--sh", "echo early; exec sleep 30"],
            b"{}",
            &[],
        );
        let took = start.elapsed();
        assert!(
            took > Duration::from_secs(4) && took < Duration::from_secs(20),
            "{took:?}"
        );
        assert_eq!(
            (text(&out.stdout), out.status.code()),
            ("early\n".into(), Some(0))
        );
        assert!(text(&out.stderr).contains("statusline: spawnSync /bin/sh ETIMEDOUT"));
        let out = run(home.path(), &["--sh", "exec sleep 30"], b"{}", &[]);
        assert_eq!((text(&out.stdout), out.status.code()), ("".into(), Some(1)));
    }

    #[test]
    fn test_a_pid_file_the_pane_stopped_touching_is_a_pane_that_is_gone() {
        let home = tempfile::tempdir().unwrap();
        let folder = open_pane(home.path(), "csr-companion", "sess-w");
        let file = fs::File::options()
            .write(true)
            .open(folder.join("panes/sess-w.json"))
            .unwrap();
        // A live pid, but no heartbeat for 2 min.
        file.set_modified(SystemTime::now() - Duration::from_secs(120))
            .unwrap();
        let out = run(home.path(), &["--sh", SH], &input(&status()), &[]);
        assert_eq!(text(&out.stdout).trim(), "drawn");
        assert!(!folder.join("statusline").exists());
    }

    #[test]
    fn test_a_dead_pid_is_a_pane_that_is_gone() {
        let home = tempfile::tempdir().unwrap();
        let folder = keep(home.path(), "csr-companion");
        fs::create_dir_all(folder.join("panes")).unwrap();
        let mut gone = Command::new("/usr/bin/true").spawn().unwrap();
        let pid = gone.id();
        gone.wait().unwrap();
        fs::write(
            folder.join("panes/sess-w.json"),
            json!({ "pid": pid }).to_string(),
        )
        .unwrap();
        let out = run(home.path(), &["--sh", SH], &input(&status()), &[]);
        assert_eq!(text(&out.stdout).trim(), "drawn");
        assert!(!folder.join("statusline").exists());
    }

    #[test]
    fn test_a_character_split_across_input_chunks_reaches_the_command_whole() {
        for pad in ["", "a"] {
            let home = tempfile::tempdir().unwrap();
            let place = format!("/repo/{pad}{}", "é".repeat(40000));
            let data = json!({"session_id": "sess-nopane", "workspace": {"current_dir": place}});
            let out = run(home.path(), &["--sh", "cat"], &input(&data), &[]);
            assert!(!text(&out.stdout).contains('\u{fffd}'));
            let back: Value = serde_json::from_slice(&out.stdout).unwrap();
            assert_eq!(
                back["workspace"]["current_dir"],
                data["workspace"]["current_dir"]
            );
        }
    }

    #[test]
    fn test_the_raw_fields_are_kept_beside_the_line_atomically() {
        let home = tempfile::tempdir().unwrap();
        let folder = open_pane(home.path(), "csr-companion", "sess-w");
        let out = run(home.path(), &["--sh", SH], &input(&status()), &[]);
        // Drawn in the pane, not under the prompt.
        assert_eq!((text(&out.stdout), out.status.code()), ("".into(), Some(0)));
        let names: Vec<_> = fs::read_dir(folder.join("statusline"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["sess-w.json"]); // no .tmp left behind
        let record = kept(&folder, "sess-w");
        assert_eq!(record["line"], "drawn\n");
        let want: Value = ["prompt_cache", "rate_limits", "context_window"]
            .iter()
            .map(|k| (k.to_string(), status()[k].clone()))
            .collect();
        assert_eq!(record["fields"], want);
        assert!(record["fields"].get("model").is_none());
        let raw = fs::read_to_string(folder.join("statusline/sess-w.json")).unwrap();
        assert!(raw.starts_with("{\"at\":") && raw.contains(",\"line\":\"drawn\\n\",\"fields\":{"));
        assert!(record["at"].as_f64().unwrap() > 1.7e9);
    }

    #[test]
    fn test_absent_fields_stay_absent() {
        let home = tempfile::tempdir().unwrap();
        let folder = open_pane(home.path(), "csr-companion", "sess-w");
        let bare = json!({"session_id": "sess-w", "rate_limits": null, "prompt_cache": "odd"});
        run(home.path(), &["--sh", SH], &input(&bare), &[]);
        assert_eq!(kept(&folder, "sess-w")["fields"], json!({}));
    }

    #[test]
    fn test_without_a_pane_the_line_passes_through_and_nothing_is_kept() {
        let home = tempfile::tempdir().unwrap();
        let out = run(home.path(), &["--sh", SH], &input(&status()), &[]);
        assert_eq!(
            (text(&out.stdout), out.status.code()),
            ("drawn\n".into(), Some(0))
        );
        assert!(!home.path().join(".claude-self-reflect").exists());
        // The line is untouched: no redaction, no newline added or removed.
        let out = run(
            home.path(),
            &["--sh", "printf 'token=abc123'"],
            &input(&status()),
            &[],
        );
        assert_eq!(text(&out.stdout), "token=abc123");
    }

    #[test]
    fn test_the_command_is_asked_for_its_compact_line_only_while_a_pane_is_open() {
        let home = tempfile::tempdir().unwrap();
        let script = "cat >/dev/null; echo \"c=$STATUSLINE_COMPACT\"";
        let out = run(home.path(), &["--sh", script], &input(&status()), &[]);
        assert_eq!(text(&out.stdout), "c=\n");
        let folder = open_pane(home.path(), "csr-companion", "sess-w");
        run(home.path(), &["--sh", script], &input(&status()), &[]);
        assert_eq!(kept(&folder, "sess-w")["line"], "c=1\n");
    }

    #[test]
    fn test_the_status_line_it_keeps_has_no_credential_in_it() {
        let home = tempfile::tempdir().unwrap();
        let folder = open_pane(home.path(), "csr-companion", "s2");
        let line = "cat >/dev/null; echo \"ctx 40% token=abc123 Bearer abc123def456\"";
        let out = run(
            home.path(),
            &["--sh", line],
            &input(&json!({"session_id": "s2"})),
            &[],
        );
        assert_eq!(out.status.code(), Some(0));
        let record = kept(&folder, "s2");
        assert_eq!(
            record["line"].as_str().unwrap().trim(),
            "ctx 40% token=[redacted] Bearer [redacted]"
        );
    }

    #[test]
    fn test_a_secret_in_a_kept_field_goes_too() {
        let home = tempfile::tempdir().unwrap();
        let folder = open_pane(home.path(), "csr-companion", "s2");
        let data = json!({"session_id": "s2", "prompt_cache": {
            "note": "key sk-0123456789abcdefghij", "password": "hunter2", "n": 5}});
        run(home.path(), &["--sh", SH], &input(&data), &[]);
        assert_eq!(
            kept(&folder, "s2")["fields"],
            json!({"prompt_cache": {"note": "key [redacted]", "password": "[redacted]", "n": 5}})
        );
    }

    #[test]
    fn test_the_shared_table_gives_the_same_answers_in_the_status_line_wrapper() {
        let pairs: Vec<[String; 2]> =
            serde_json::from_str(include_str!("fixtures/companion/redaction_table.json")).unwrap();
        let home = tempfile::tempdir().unwrap();
        let folder = open_pane(home.path(), "csr-companion", "s2");
        for [said, want] in pairs {
            let out = run(
                home.path(),
                &["--sh", "cat >/dev/null; printf %s \"$SAID\""],
                &input(&json!({"session_id": "s2"})),
                &[("SAID", &said)],
            );
            let code = if said.is_empty() { 1 } else { 0 };
            assert_eq!(out.status.code(), Some(code), "{said}");
            assert_eq!(kept(&folder, "s2")["line"], want, "{said}");
        }
    }

    #[test]
    fn test_the_kept_file_is_0600_in_0700_folders_and_an_old_one_is_tightened() {
        let home = tempfile::tempdir().unwrap();
        let folder = open_pane(home.path(), "csr-companion", "s1");
        fs::create_dir_all(folder.join("statusline")).unwrap();
        let old = folder.join("statusline/s1.json");
        fs::write(&old, "{}").unwrap();
        for dir in [&folder, &folder.join("statusline")] {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::set_permissions(&old, fs::Permissions::from_mode(0o644)).unwrap();
        let out = run(
            home.path(),
            &["--sh", SH],
            &input(&json!({"session_id": "s1"})),
            &[],
        );
        assert_eq!(out.status.code(), Some(0));
        let modes = [mode(&folder), mode(&folder.join("statusline")), mode(&old)];
        assert_eq!(modes, [0o700, 0o700, 0o600]);
        assert_eq!(
            kept(&folder, "s1")["line"].as_str().unwrap().trim(),
            "drawn"
        ); // rewritten
    }

    #[test]
    fn test_the_wrapper_does_not_write_through_a_link_either() {
        let home = tempfile::tempdir().unwrap();
        let folder = open_pane(home.path(), "csr-companion", "s3");
        let elsewhere = home.path().join("outside/folder");
        fs::create_dir_all(&elsewhere).unwrap();
        fs::set_permissions(&elsewhere, fs::Permissions::from_mode(0o755)).unwrap();
        symlink(&elsewhere, folder.join("statusline")).unwrap();
        let data = input(&json!({"session_id": "s3"}));
        let out = run(home.path(), &["--sh", SH], &data, &[]);
        assert_eq!(out.status.code(), Some(0));
        assert_eq!(
            (fs::read_dir(&elsewhere).unwrap().count(), mode(&elsewhere)),
            (0, 0o755)
        );
        // It fell back to the prompt line.
        assert_eq!(text(&out.stdout).trim(), "drawn");
        // A link in the file's own place is refused too, its target left alone.
        fs::remove_file(folder.join("statusline")).unwrap();
        fs::create_dir(folder.join("statusline")).unwrap();
        let target = home.path().join("outside/target");
        fs::write(&target, "keep").unwrap();
        symlink(&target, folder.join("statusline/s3.json")).unwrap();
        let out = run(home.path(), &["--sh", SH], &data, &[]);
        assert_eq!(text(&out.stdout).trim(), "drawn");
        assert_eq!(fs::read_to_string(target).unwrap(), "keep");
    }

    #[test]
    fn test_a_session_id_that_is_not_a_word_never_finds_a_pane() {
        let home = tempfile::tempdir().unwrap();
        let folder = open_pane(home.path(), "csr-companion", "a.b");
        for id in [json!("a.b"), json!("../x"), json!(""), json!(null)] {
            let out = run(
                home.path(),
                &["--sh", SH],
                &input(&json!({"session_id": id})),
                &[],
            );
            assert_eq!(text(&out.stdout).trim(), "drawn", "{id}");
        }
        assert!(!folder.join("statusline").exists());
        let out = run(home.path(), &["--sh", SH], b"not json", &[]);
        assert_eq!(text(&out.stdout).trim(), "drawn");
    }

    #[test]
    fn test_the_dev_mods_keep_wins_and_a_pane_in_only_the_plugins_keep_still_counts() {
        let home = tempfile::tempdir().unwrap();
        let plugin = open_pane(home.path(), "csr-companion", "s1");
        let data = input(&json!({"session_id": "s1"}));
        run(home.path(), &["--sh", SH], &data, &[]);
        assert!(plugin.join("statusline/s1.json").exists());
        let shifts = open_pane(home.path(), "csr-shifts", "s1");
        fs::remove_file(plugin.join("statusline/s1.json")).unwrap();
        run(home.path(), &["--sh", SH], &data, &[]);
        assert!(shifts.join("statusline/s1.json").exists());
        assert!(!plugin.join("statusline/s1.json").exists());
    }
}
