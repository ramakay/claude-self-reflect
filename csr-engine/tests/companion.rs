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
    // *.db is gitignored, so the DB is built from home.sql, never copied.
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
/// The home the main-*.txt goldens were drawn from: pane.py's draw_main over a hand-built data dict with no
/// agents and no Codex runs (oracle.py). In flight is checked in companion_frame.rs, in the rich world.
fn bare_home() -> (tempfile::TempDir, Paths) {
    let (d, paths) = home();
    let _ = fs::remove_dir_all(d.path().join(".codex"));
    let _ = fs::remove_dir_all(paths.keep.join("agents"));
    // ... and no compaction of CSR's, so no "this session:" row under the masthead (the rich world checks it)
    let dreams = paths.keep.join("dreams.jsonl");
    let kept: String = fs::read_to_string(&dreams)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.contains("\"kind\": \"compact\""))
        .map(|line| format!("{line}\n"))
        .collect();
    fs::write(&dreams, kept).unwrap();
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
fn golden(path: PathBuf, actual: &str) -> String {
    if std::env::var_os("CSR_COMPANION_UPDATE").is_some() {
        fs::write(&path, actual).unwrap();
    }
    fs::read_to_string(path).unwrap()
}

#[test]
fn frame_goldens_are_byte_equal() {
    for (cols, rows) in [(46, 30), (100, 40)] {
        for page in ["main", "log"] {
            let (_home, paths) = bare_home();
            let mut pane = Pane::new(paths, &args(page, cols, rows));
            pane.pixels = false;
            let actual = pane.frame(cols, rows, 1792320000.).join("\n") + "\n";
            let expected = golden(fixture().join(format!("{page}-{cols}x{rows}.txt")), &actual);
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
            "nonesuch",
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
        .find(|(_, a)| a["key"] == "instrument:dreams")
        .unwrap()
        .0;
    assert!(pane.click(row, 6, 1792320000.));
    assert!(pane.instrument_open.dreams);
    let detail = pane
        .frame(100, 40, 1792320000.)
        .iter()
        .map(|line| csr_engine::companion::render::text::plain(line))
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        detail.contains("Keep the heartbeat independent of the renderer."),
        "{detail}"
    );
    assert!(pane.key("escape"));
    assert!(!pane.instrument_open.dreams);
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
    assert_eq!(answer["id"], "newest");
    assert_eq!(answer["answer"], "helpful");
    assert!(pane.key("l"));
    assert_eq!(pane.view, "main");
    assert!(pane.instrument_open.changes);
    assert!(pane.key("d"));
    assert_eq!(pane.view, "main");
    assert!(pane.instrument_open.dreams && !pane.instrument_open.changes);
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
    let frame_log = paths.home.join("frames.log");
    let child = Command::new(executable)
        .env("HOME", &paths.home)
        .env("TERM_PROGRAM", "kitty")
        .env("CSR_PANE_FRAME_LOG", &frame_log)
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
    let kept: serde_json::Value = serde_json::from_slice(&fs::read(&pid).unwrap()).unwrap();
    assert_eq!(kept["pid"], child_guard.0.id());
    // The pane listens at a pipe of its own; the relay trusts no pid.
    let wakes = paths
        .keep
        .join(format!("panes/sess-now.{}.wake", child_guard.0.id()));
    assert!(
        wakes.exists(),
        "the pane keeps a pipe for the relay's wakes"
    );
    // Let the first frames land and the pane go quiet; away from a minute edge, whose clock
    // labels would redraw it for their own reasons.
    let seconds = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            % 60
    };
    let drain = |master: &mut fs::File, output: &mut Vec<u8>, span: Duration| {
        let until = Instant::now() + span;
        while Instant::now() < until {
            let _ = master.read_to_end(output);
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    // Quiet: the pane has drawn and written all of it, and a second and a half passed with
    // nothing more.
    let quiet = |master: &mut fs::File, output: &mut Vec<u8>| {
        let mut changed = Instant::now();
        let mut length = 0;
        let began = Instant::now();
        while changed.elapsed() < Duration::from_millis(1500) {
            assert!(
                began.elapsed() < Duration::from_secs(30),
                "the pane never went quiet"
            );
            drain(master, output, Duration::from_millis(50));
            if output.len() != length {
                length = output.len();
                changed = Instant::now();
            }
        }
    };
    let frames = |log: &std::path::Path| fs::read_to_string(log).map_or(0, |t| t.lines().count());
    // The log records a frame when it is written out, not when it begins: wait for the first one
    // (a loaded machine takes a while) before judging anything quiet.
    let began = Instant::now();
    while frames(&frame_log) == 0 {
        assert!(
            began.elapsed() < Duration::from_secs(60),
            "the pane never finished its first frame"
        );
        drain(&mut master, &mut output, Duration::from_millis(50));
    }
    quiet(&mut master, &mut output);
    while seconds() >= 52 {
        drain(&mut master, &mut output, Duration::from_millis(500));
    }
    let settled = frames(&frame_log);
    // Fifty wakes of each kind in half a second, written to the live pane: they draw it once at
    // the leading edge and once more for the rest, a second later; never fifty times. A loaded
    // machine can stretch the burst past the spacing, which earns one more draw per spacing.
    let burst = Instant::now();
    for _ in 0..50 {
        assert!(csr_engine::companion::wake::notify(
            &paths.keep,
            "sess-now",
            csr_engine::companion::wake::Kind::Now
        ));
        assert!(csr_engine::companion::wake::notify(
            &paths.keep,
            "sess-now",
            csr_engine::companion::wake::Kind::Burst
        ));
        drain(&mut master, &mut output, Duration::from_millis(10));
    }
    let burst = burst.elapsed();
    let stretched =
        (burst.as_secs_f64() / csr_engine::companion::wake::SPACING.as_secs_f64()) as usize;
    // The first wake's frame, then the frames' count unchanged for longer than the spacing and a
    // slow build together.
    let began = Instant::now();
    while frames(&frame_log) == settled {
        assert!(
            began.elapsed() < Duration::from_secs(60),
            "100 wakes drew no frame"
        );
        drain(&mut master, &mut output, Duration::from_millis(50));
    }
    let mut count = frames(&frame_log);
    let mut changed = Instant::now();
    while changed.elapsed() < Duration::from_millis(3000) {
        drain(&mut master, &mut output, Duration::from_millis(50));
        if frames(&frame_log) != count {
            count = frames(&frame_log);
            changed = Instant::now();
        }
    }
    let drawn = frames(&frame_log) - settled;
    assert!(
        (1..=2 + stretched).contains(&drawn),
        "100 wakes drew {drawn} frames, want 1 to {} (burst took {:?}, settled at {settled}, log {:?})",
        2 + stretched,
        burst,
        fs::read_to_string(&frame_log)
    );
    assert!(
        child_guard.0.try_wait().unwrap().is_none(),
        "a burst of wakes must not end the pane"
    );
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
            // Drain the whole-pane PNG promptly, as a terminal does. A slow PTY reader
            // otherwise blocks stdout before watch can poll the replacement binary.
            std::thread::sleep(Duration::from_millis(1));
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
            let (_home, paths) = bare_home();
            let mut pane = Pane::new(paths, &args(page, cols, rows));
            pane.pixels = true;
            pane.enable_cards();
            let actual = pane.frame(cols, rows, 1792320000.).join("\n") + "\n";
            let expected = golden(
                fixture().join(format!("pixels-{page}-{cols}x{rows}.txt")),
                &actual,
            );
            for (i, (a, b)) in actual.lines().zip(expected.lines()).enumerate() {
                assert_eq!(a, b, "pixels {page} {cols}x{rows}, row {}", i + 1);
            }
            assert_eq!(actual, expected);
            assert!(!pane.placed.is_empty());
            assert!(pane.placed.iter().all(|p| p.row + p.rows < rows));
            if page == "main" {
                assert_eq!(pane.placed.len(), 1);
                assert_eq!(pane.placed[0].cols, cols);
                assert!(pane.instrument_metrics.is_some());
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
    // These three draw text. Their goldens are in the Mac's Menlo, as the pane draws
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
    let (_home, paths) = bare_home();
    let output = tempfile::tempdir().unwrap();
    fn snapshot(root: &std::path::Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
        fn visit(
            root: &std::path::Path,
            at: &std::path::Path,
            found: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>,
        ) {
            for entry in fs::read_dir(at).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    visit(root, &entry.path(), found);
                } else {
                    found.insert(
                        entry.path().strip_prefix(root).unwrap().into(),
                        fs::read(entry.path()).unwrap(),
                    );
                }
            }
        }
        let mut found = std::collections::BTreeMap::new();
        visit(root, root, &mut found);
        found
    }
    let before = snapshot(&paths.home);
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
    let mut screen_args = args("main", 44, 30);
    screen_args.png = Some(output.path().into());
    let mut screen_pane = Pane::new(paths.clone(), &screen_args);
    screen_pane.pixels = true;
    let expected = screen_pane.frame(44, 30, 1792320000.).join("\n") + "\n";
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
    use csr_engine::companion::render::{kitty, theme::Theme};
    let cell = kitty::cell_size();
    let check = |cols: usize, rows: usize| {
        for (name, theme) in [("dark", Theme::Dark), ("light", Theme::Light)] {
            let image = image::open(output.path().join(format!("instrument-{name}.png")))
                .unwrap()
                .to_rgba8();
            assert_eq!(
                image.dimensions(),
                (cols as u32 * cell.0, rows as u32 * cell.1)
            );
            let mut shot = args("main", cols, rows);
            shot.png = Some(output.path().into());
            let mut pane = Pane::new(paths.clone(), &shot);
            pane.theme = theme;
            pane.pixels = true;
            pane.enable_cards();
            let lines = pane.frame(cols, rows, 1792320000.);
            assert!(
                !pane.placed.is_empty(),
                "instrument must export actual pane pixels"
            );
            for placement in &pane.placed {
                assert!(placement.col + placement.cols <= cols);
                assert!(placement.row + placement.rows < rows);
                let source = image::load_from_memory(&placement.png).unwrap().to_rgba8();
                let source = image::imageops::resize(
                    &source,
                    placement.cols as u32 * cell.0,
                    placement.rows as u32 * cell.1,
                    image::imageops::FilterType::Lanczos3,
                );
                let mut expected = image::RgbaImage::from_pixel(
                    source.width(),
                    source.height(),
                    image::Rgba(theme.tokens().ground),
                );
                image::imageops::overlay(&mut expected, &source, 0, 0);
                let region = image::imageops::crop_imm(
                    &image,
                    placement.col as u32 * cell.0,
                    placement.row as u32 * cell.1,
                    source.width(),
                    source.height(),
                )
                .to_image();
                assert_eq!(region, expected, "placement pixels at {cols}x{rows} {name}");
            }
            assert!(lines.last().unwrap().contains("q "));
            let footer = image::imageops::crop_imm(
                &image,
                0,
                (rows as u32 - 1) * cell.1,
                image.width(),
                cell.1,
            )
            .to_image();
            assert!(
                footer.pixels().any(|p| p.0 != theme.tokens().ground),
                "footer text must be drawn"
            );
        }
    };
    check(44, 30);
    for (cols, rows) in [(44, 30), (60, 40)] {
        let out = Command::new(env!("CARGO_BIN_EXE_csr-engine"))
            .env("HOME", &paths.home)
            .env("TERM_PROGRAM", "kitty")
            .env("CSR_COMPANION_NOW", "1792320000")
            .env_remove("CSR_COMPANION_PIXELS")
            .args([
                "companion",
                "pane",
                "--repo",
                args("main", cols, rows).repo.to_str().unwrap(),
                "--sid",
                "sess-now",
                "--png",
                output.path().to_str().unwrap(),
                "--cols",
                &cols.to_string(),
                "--rows",
                &rows.to_string(),
            ])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        check(cols, rows);
    }
    assert_eq!(
        snapshot(&paths.home),
        before,
        "PNG export must leave HOME untouched"
    );
}

#[test]
fn disabling_pixels_keeps_text_strips_even_in_kitty() {
    let (_home, paths) = bare_home();
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
    let actual = String::from_utf8(out.stdout).unwrap();
    assert!(!actual.contains("\x1b_G"));
    assert!(actual.contains("What if a frame failed"));
    assert!(actual.contains("CTX "));
    assert_eq!(actual, golden(fixture().join("main-46x30.txt"), &actual));
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

    /// Run the engine once and wait for it to exit, so the timed phases below measure the lease
    /// and not the first run of a freshly built binary: macOS scans a new file the first time it
    /// runs, which cost 1.5 to 4.9 s on the dev laptop against these tests' 5 s waits.
    fn warm() {
        static WARM: std::sync::Once = std::sync::Once::new();
        WARM.call_once(|| {
            let status = Command::new(env!("CARGO_BIN_EXE_csr-engine"))
                .arg("--version")
                .stdout(Stdio::null())
                .status()
                .unwrap();
            assert!(status.success());
        });
    }

    fn start(folder: &std::path::Path, root: &str, pause: &str) -> Child {
        warm();
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

mod relay {
    use serde_json::{json, Value};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::process::{Child, ChildStdout, Command, Stdio};
    use std::time::{Duration, Instant};

    struct Serve {
        child: Child,
        lines: Option<BufReader<ChildStdout>>,
        sock: PathBuf,
        _dir: tempfile::TempDir,
    }

    impl Serve {
        fn line(&mut self) -> Value {
            let mut line = String::new();
            self.lines.as_mut().unwrap().read_line(&mut line).unwrap();
            serde_json::from_str(&line).unwrap()
        }
    }

    impl Drop for Serve {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// The version the relay posts: the engine checks it against its own build.
    const V: &str = env!("CARGO_PKG_VERSION");
    /// A relay of some other build.
    const OTHER: &str = "0.0.0-other";

    fn start(relay: &str, extra_env: &[(&str, &Path)]) -> Serve {
        let dir = tempfile::Builder::new()
            .prefix("csr-t")
            .tempdir_in("/tmp")
            .unwrap();
        let sock = dir.path().join("run/a.sock");
        let mut command = Command::new(env!("CARGO_BIN_EXE_csr-engine"));
        command
            .env("HOME", dir.path())
            .args(["companion", "serve", "--sock"])
            .arg(&sock)
            .args(["--relay", relay])
            .stdout(Stdio::piped());
        for (key, value) in extra_env {
            command.env(key, value);
        }
        let mut child = command.spawn().unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap());
        let mut first = String::new();
        lines.read_line(&mut first).unwrap();
        assert_eq!(first, "ready\n");
        Serve {
            child,
            lines: Some(lines),
            sock,
            _dir: dir,
        }
    }

    fn request(stream: &mut UnixStream, path: &str, body: &Value) -> Value {
        let body = body.to_string();
        write!(
            stream,
            "POST {path} HTTP/1.1\r\nHost: csr\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            head.push(byte[0]);
        }
        let head = String::from_utf8(head).unwrap().to_ascii_lowercase();
        assert!(head.starts_with("http/1.1 200"), "{head}");
        let length = head
            .lines()
            .find_map(|l| l.strip_prefix("content-length: "))
            .unwrap()
            .trim()
            .parse::<usize>()
            .unwrap();
        let mut reply = vec![0u8; length];
        stream.read_exact(&mut reply).unwrap();
        serde_json::from_slice(&reply).unwrap()
    }

    fn post(sock: &Path, path: &str, body: &Value) -> Value {
        request(&mut UnixStream::connect(sock).unwrap(), path, body)
    }

    fn exited(child: &mut Child, within: Duration) -> Option<std::process::ExitStatus> {
        let until = Instant::now() + within;
        while Instant::now() < until {
            if let Some(status) = child.try_wait().unwrap() {
                return Some(status);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    }

    #[test]
    fn serve_is_private_answers_csr_and_removes_its_socket_on_sigterm() {
        let mut serve = start("1", &[]);
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&serve.sock), 0o600);
        assert_eq!(mode(serve.sock.parent().unwrap()), 0o700);
        let reply = post(
            &serve.sock,
            "/event",
            &json!({ "v": V, "k": "tool", "e": {} }),
        );
        assert_eq!(reply, json!({}));
        let reply = post(
            &serve.sock,
            "/event",
            &json!({ "v": V, "k": "csr", "e": {} }),
        );
        let pid = serve.child.id();
        assert_eq!(
            reply["answer"]["text"],
            format!("csr relay ok pid={pid} events=2")
        );
        assert_eq!(unsafe { libc::kill(pid as i32, libc::SIGTERM) }, 0);
        let status = exited(&mut serve.child, Duration::from_secs(5)).expect("exits on SIGTERM");
        assert!(status.success(), "{status:?}");
        assert!(!serve.sock.exists());
    }

    #[test]
    fn serve_exits_on_sigint_too() {
        let mut serve = start("1", &[]);
        assert_eq!(
            unsafe { libc::kill(serve.child.id() as i32, libc::SIGINT) },
            0
        );
        assert!(exited(&mut serve.child, Duration::from_secs(5))
            .unwrap()
            .success());
        assert!(!serve.sock.exists());
    }

    #[test]
    fn a_dead_socket_file_is_replaced_and_a_regular_file_is_refused() {
        let dir = tempfile::Builder::new()
            .prefix("csr-t")
            .tempdir_in("/tmp")
            .unwrap();
        let run = dir.path().join("run");
        std::fs::create_dir(&run).unwrap();
        let sock = run.join("a.sock");
        drop(std::os::unix::net::UnixListener::bind(&sock).unwrap());
        let serve = |sock: &Path| {
            Command::new(env!("CARGO_BIN_EXE_csr-engine"))
                .env("HOME", dir.path())
                .args(["companion", "serve", "--sock"])
                .arg(sock)
                .args(["--relay", "1"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        };
        let mut child = serve(&sock);
        let mut ready = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready, "ready\n");
        post(&sock, "/event", &json!({ "v": V, "k": "tool" }));
        child.kill().unwrap();
        child.wait().unwrap();
        let file = run.join("b.sock");
        std::fs::write(&file, "keep").unwrap();
        let out = serve(&file).wait_with_output().unwrap();
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("not a socket"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "keep");
    }

    #[test]
    fn a_different_relay_version_pushes_one_toast_on_stdout() {
        let mut serve = start("2", &[]);
        post(&serve.sock, "/event", &json!({ "v": OTHER, "k": "tool" }));
        let pushed = serve.line();
        assert_eq!(pushed["calls"][0]["m"], "ui.toast");
        assert_eq!(
            pushed["calls"][0]["a"][0],
            "CSR updated: run /reload-plugins"
        );
        // A second mismatch pushes nothing; the next line is the usage call that a start asks.
        post(&serve.sock, "/event", &json!({ "v": OTHER, "k": "tool" }));
        let start = post(
            &serve.sock,
            "/event",
            &json!({ "v": V, "k": "start", "e": { "isInteractive": true } }),
        );
        post(
            &serve.sock,
            "/step",
            &json!({ "v": V, "id": start["id"], "results": [] }),
        );
        assert_eq!(serve.line()["calls"][0]["m"], "session.usage");
    }

    #[test]
    fn start_then_step_pushes_a_usage_call_and_logs_what_comes_back() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("relay.log");
        let mut serve = start("1", &[("CSR_RELAY_LOG", &log)]);
        let reply = post(
            &serve.sock,
            "/event",
            &json!({ "v": V, "sid": "s1", "k": "start",
                "e": { "cwd": "/r", "isInteractive": true } }),
        );
        assert_eq!(reply["calls"][0]["m"], "command.register");
        let done = post(
            &serve.sock,
            "/step",
            &json!({ "v": V, "id": reply["id"], "results": [{ "id": "c1", "v": {} }] }),
        );
        assert_eq!(done, json!({}));
        let pushed = serve.line();
        assert_eq!(pushed["calls"][0]["m"], "session.usage");
        post(
            &serve.sock,
            "/step",
            &json!({ "v": V, "id": pushed["id"], "results": [{ "id": "c1", "v": { "x": 1 } }] }),
        );
        let rows: Vec<Value> = std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        // boot, the start request, the first step, what the usage call came back with, its step.
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0]["k"], "boot");
        assert_eq!(rows[0]["pid"], serve.child.id());
        assert_eq!(rows[1]["k"], "start");
        assert_eq!(rows[1]["sid"], "s1");
        assert_eq!(rows[1]["pid"], serve.child.id());
        assert_eq!(rows[1]["e"], json!({ "cwd": "/r", "isInteractive": true }));
        assert!(rows[1]["bytes"].as_u64().unwrap() > 0);
        assert!(rows[1]["handle_us"].is_u64());
        assert_eq!(rows[3]["k"], "usage");
        assert_eq!(rows[3]["usage"], json!({ "x": 1 }));
    }

    /// What the engine pushes on stdout, read apart so a test can wait for it with a time limit.
    fn listen(serve: &mut Serve) -> std::sync::mpsc::Receiver<Value> {
        let mut lines = serve.lines.take().unwrap();
        let (send, got) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            loop {
                line.clear();
                if lines.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let Ok(pushed) = serde_json::from_str(&line) else {
                    continue;
                };
                if send.send(pushed).is_err() {
                    break;
                }
            }
        });
        got
    }

    /// What Claude Code says to a call of the engine's: a long conversation, where an empty
    /// history pays a compaction back in 14 calls, the compactor on auto, a terminal to draw on;
    /// anything else nothing.
    fn said(call: &Value) -> Value {
        match call["m"].as_str().unwrap_or("") {
            "session.usage" => json!({ "startedAt": 0, "rateLimits": [],
                "context": { "tokens": 260_000, "window": 1_000_000, "percent": 26 } }),
            "config.list" => json!([{ "key": "csr-companion.autoCompact", "kind": "enum",
                "value": "auto" }]),
            "session.surfaces" => json!(["terminal"]),
            _ => Value::Null,
        }
    }

    /// A reply that holds calls is answered, and its answer too, until one holds none.
    fn settle(sock: &Path, mut reply: Value) -> Value {
        while let Some(calls) = reply["calls"].as_array() {
            let results: Vec<Value> = (calls.iter())
                .map(|call| json!({ "id": call["id"], "v": said(call) }))
                .collect();
            let body = json!({ "v": V, "id": reply["id"], "results": results });
            reply = post(sock, "/step", &body);
        }
        reply
    }

    /// Answer what the engine pushes until `done` holds, for at most ten seconds.
    fn pump(sock: &Path, pushed: &std::sync::mpsc::Receiver<Value>, done: impl Fn() -> bool) {
        let limit = Instant::now() + Duration::from_secs(10);
        while !done() && Instant::now() < limit {
            if let Ok(conversation) = pushed.recv_timeout(Duration::from_millis(20)) {
                settle(sock, conversation);
            }
        }
        assert!(done(), "the engine did not get there");
    }

    /// The text and the buttons (label, key) of a tree.
    fn drawn(tree: &Value, texts: &mut Vec<String>, buttons: &mut Vec<(String, String)>) {
        if tree["type"] == "Button" {
            let (label, key) = (&tree["props"]["label"], &tree["props"]["key"]);
            buttons.push((label.as_str().unwrap().into(), key.as_str().unwrap().into()));
        }
        for child in tree["children"].as_array().into_iter().flatten() {
            match child.as_str() {
                Some(text) => texts.push(text.to_string()),
                None => drawn(child, texts, buttons),
            }
        }
    }

    fn band(sock: &Path, sid: &str) -> Value {
        let e = json!({ "component": "AbovePrompt", "props": {}, "beneath": null });
        let reply = post(
            sock,
            "/event",
            &json!({ "v": V, "sid": sid, "cwd": "/repo", "k": "band", "e": e }),
        );
        settle(sock, reply)
    }

    fn press(sock: &Path, sid: &str, key: &str) -> Value {
        let body = json!({ "v": V, "sid": sid, "cwd": "/repo", "key": key, "requestId": "r1" });
        settle(sock, post(sock, "/press", &body))
    }

    fn button_of(tree: &Value, label: &str) -> String {
        let (mut texts, mut buttons) = (Vec::new(), Vec::new());
        drawn(tree, &mut texts, &mut buttons);
        let found = buttons.iter().find(|(shown, _)| shown == label);
        found
            .unwrap_or_else(|| panic!("no button {label} in {buttons:?}"))
            .1
            .clone()
    }

    #[test]
    fn the_band_draws_a_remembered_note_and_a_countdown_s_cancel_reaches_the_compactor() {
        use csr_engine::companion::store::{Store, MACHINE};
        let mut serve = start("1", &[]);
        let pushed = listen(&mut serve);
        let root = serve._dir.path().join(".claude-self-reflect/csr-companion");
        let store = Store::at(root.join("companion.db"));
        let note = json!({
            "id": "shift:src/hooks/mod.rs:ca5655a841fb:d8fe3a96", "kind": "shift",
            "file": "src/hooks/mod.rs",
            "line": "`dispatch_hook` in mod.rs changed in ca5655a on 09-17.",
            "title": "Your 09-14 work on dispatch_hook has changed since",
            "text": "On 09-14 you asked “switch to main”.", "sketch": [],
            "oid": "ca5655a841fb", "conversation": "d8fe3a96-e6f8", "at": 1,
        });
        let up = json!({ "kind": "offer", "note": note, "tag": "Heads up", "shownAt": 1,
            "promptsSurvived": 0 });
        store.set("s1", "view", &up).unwrap();

        // A session with a remembered note: the band draws the note and its buttons.
        let tree = band(&serve.sock, "s1");
        let (mut texts, mut buttons) = (Vec::new(), Vec::new());
        drawn(&tree["tree"], &mut texts, &mut buttons);
        assert!(texts.iter().any(|t| t == "Heads up · "), "{texts:?}");
        assert!(
            texts.iter().any(|t| t.contains("changed in ca5655a")),
            "{texts:?}"
        );
        let labels: Vec<&str> = buttons.iter().map(|(label, _)| label.as_str()).collect();
        assert_eq!(labels, ["Learn more", "Knew this already", "Dismiss"]);

        // Knew this already: the press is answered, and the note never shows again.
        let key = button_of(&tree["tree"], "Knew this already");
        assert_eq!(press(&serve.sock, "s1", &key), json!({}));
        assert_eq!(
            store.get(MACHINE, "known").unwrap().unwrap(),
            json!([note["id"]])
        );
        assert_eq!(store.get("s1", "view").unwrap(), None);
        assert_eq!(band(&serve.sock, "s1"), json!({}));
        assert_eq!(
            press(&serve.sock, "s1", &key),
            json!({}),
            "a late press is nothing"
        );

        // A turn that ends high on context, on auto: a countdown, with its Cancel on the band.
        let start = post(
            &serve.sock,
            "/event",
            &json!({ "v": V, "sid": "s2", "cwd": "/repo", "k": "start",
                "e": { "cwd": "/repo", "isInteractive": true } }),
        );
        settle(&serve.sock, start);
        let notify = |k: &str, e: Value| {
            let body = json!({ "v": V, "sid": "s2", "cwd": "/repo", "k": k, "e": e });
            post(&serve.sock, "/event", &body)
        };
        notify("turn_start", json!({ "text": "", "turnId": "t" }));
        notify(
            "stop",
            json!({ "stop_hook_active": false, "background_tasks": [] }),
        );
        let landed = "Done. Codex committed 6369e6b on the study branch; the gates are green.";
        notify(
            "turn_complete",
            json!({ "answer": landed, "durationMs": 1000, "isAborted": false, "turnId": "t",
                "reason": "answer" }),
        );
        let counting = || {
            store
                .get("s2", "compact")
                .ok()
                .flatten()
                .is_some_and(|v| v["kind"] == "countdown")
        };
        pump(&serve.sock, &pushed, counting);
        let tree = band(&serve.sock, "s2");
        let (mut texts, mut buttons) = (Vec::new(), Vec::new());
        drawn(&tree["tree"], &mut texts, &mut buttons);
        assert!(
            texts.iter().any(|t| t.starts_with("Compacting in")),
            "{texts:?}"
        );
        let key = button_of(&tree["tree"], "Cancel");
        assert_eq!(key, "compact-cancel");

        // The band's press goes to the compactor: the countdown is over, and the log says who.
        assert_eq!(press(&serve.sock, "s2", &key), json!({}));
        pump(&serve.sock, &pushed, || !counting());
        let log = root.join("dreams.jsonl");
        let cancelled = || {
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            text.lines()
                .any(|l| l.contains("\"event\":\"cancel\"") && l.contains("\"by\":\"press\""))
        };
        pump(&serve.sock, &pushed, cancelled);
        let after = band(&serve.sock, "s2");
        let (mut texts, mut buttons) = (Vec::new(), Vec::new());
        drawn(&after["tree"], &mut texts, &mut buttons);
        assert!(
            !texts.iter().any(|t| t.starts_with("Compacting")),
            "{texts:?}"
        );
    }

    #[test]
    fn a_step_for_an_id_nobody_waits_for_replies_empty() {
        let serve = start("1", &[]);
        for body in [
            json!({ "v": V, "id": "e404", "results": [] }),
            json!({ "v": V, "results": [{ "id": "c1", "v": 1 }] }),
        ] {
            assert_eq!(post(&serve.sock, "/step", &body), json!({}));
        }
    }

    #[test]
    fn a_respawned_start_does_not_ask_for_the_command_again() {
        let serve = start("1", &[]);
        let first = post(
            &serve.sock,
            "/event",
            &json!({ "v": V, "sid": "s1", "cwd": "/r", "k": "start",
                "e": { "isInteractive": true } }),
        );
        assert_eq!(first["calls"][0]["m"], "command.register");
        let again = post(
            &serve.sock,
            "/event",
            &json!({ "v": V, "sid": "s1", "cwd": "/r", "k": "start", "respawn": true,
                "e": { "isInteractive": true } }),
        );
        assert_eq!(again, json!({}));
    }

    #[test]
    fn serve_clears_stale_sockets_from_its_folder_and_leaves_the_rest() {
        let dir = tempfile::Builder::new()
            .prefix("csr-t")
            .tempdir_in("/tmp")
            .unwrap();
        let run = dir.path().join("run");
        std::fs::create_dir(&run).unwrap();
        let dead = run.join("old-1a.sock");
        drop(std::os::unix::net::UnixListener::bind(&dead).unwrap());
        let live = run.join("live-1a.sock");
        let _live = std::os::unix::net::UnixListener::bind(&live).unwrap();
        let note = run.join("note.txt");
        std::fs::write(&note, "keep").unwrap();
        // A socket younger than two seconds may belong to a child still starting up.
        std::thread::sleep(Duration::from_millis(2200));
        let mut child = Command::new(env!("CARGO_BIN_EXE_csr-engine"))
            .env("HOME", dir.path())
            .args(["companion", "serve", "--sock"])
            .arg(run.join("new-1a.sock"))
            .args(["--relay", "1"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut ready = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready, "ready\n");
        let kept = (dead.exists(), live.exists(), note.exists());
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(kept, (false, true, true));
    }

    #[test]
    fn serve_exits_when_a_push_meets_a_closed_stdout() {
        let mut serve = start("2", &[]);
        drop(serve.lines.take());
        let sock = serve.sock.clone();
        post(&sock, "/event", &json!({ "v": OTHER, "k": "tool" }));
        let status = exited(&mut serve.child, Duration::from_secs(5)).expect("exits on EPIPE");
        assert!(status.success(), "{status:?}");
        assert!(!sock.exists());
    }

    #[test]
    fn serve_exits_when_stdout_is_closed_and_nothing_is_pushed() {
        let mut serve = start("1", &[]);
        drop(serve.lines.take());
        let status = exited(&mut serve.child, Duration::from_secs(5)).expect("exits when idle");
        assert!(status.success(), "{status:?}");
        assert!(!serve.sock.exists());
    }

    #[test]
    fn the_relay_round_trip_stays_well_under_fifteen_ms() {
        let serve = start("1", &[]);
        let measure = |fresh: bool| {
            let mut kept = UnixStream::connect(&serve.sock).unwrap();
            let mut times = Vec::new();
            for i in 0..1000 {
                let body = json!({ "v": V, "sid": "s", "cwd": "/r", "k": "tool",
                    "e": { "tool": "Bash", "n": i, "input": { "command": "ls -la" } } });
                let started = Instant::now();
                if fresh {
                    post(&serve.sock, "/event", &body);
                } else {
                    request(&mut kept, "/event", &body);
                }
                times.push(started.elapsed().as_secs_f64() * 1000.);
            }
            times.sort_by(f64::total_cmp);
            (times[499], times[949], times[999])
        };
        for (name, fresh) in [("keep-alive", false), ("new connection", true)] {
            let (p50, p95, max) = measure(fresh);
            eprintln!(
                "POST /event x1000 ({name}): p50 {p50:.3} ms, p95 {p95:.3} ms, max {max:.3} ms"
            );
            assert!(p95 < 15., "{name} p95 {p95} ms");
        }
    }

    /// The relay alone, under `claude plugin test`: `$.http.fetch` and `$.process.spawn` are
    /// scripted by the test kit, so the transport runs with no engine behind it.
    #[test]
    fn the_relay_passes_its_plugin_test_cases() {
        let home = tempfile::tempdir().unwrap();
        // The CLI writes under its HOME (.claude.json, .claude/backups): never the real one.
        let Ok(found) = Command::new("claude")
            .env("HOME", home.path())
            .arg("--version")
            .output()
        else {
            eprintln!("claude is not on PATH: relay cases skipped");
            return;
        };
        if !found.status.success() {
            return;
        }
        let out = tempfile::tempdir().unwrap();
        let emit = Command::new(env!("CARGO_BIN_EXE_csr-engine"))
            .env("HOME", home.path())
            .args(["companion", "plugin", "emit"])
            .arg(out.path())
            .output()
            .unwrap();
        assert!(emit.status.success());
        std::fs::create_dir(out.path().join("tests")).unwrap();
        std::fs::write(
            out.path().join("tests/relay.test.ts"),
            include_str!("fixtures/relay/relay.test.ts"),
        )
        .unwrap();
        let ran = Command::new("claude")
            .env("HOME", home.path())
            .env_remove("CLAUDE_CONFIG_DIR")
            .args(["plugin", "test"])
            .arg(out.path())
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&ran.stdout).to_string()
            + &String::from_utf8_lossy(&ran.stderr);
        assert!(ran.status.success(), "{text}");
        assert!(text.contains(" 0 fail"), "{text}");
        let passed: usize = text
            .lines()
            .find_map(|l| l.trim().strip_suffix(" pass"))
            .and_then(|n| n.trim().parse().ok())
            .unwrap_or(0);
        assert_eq!(passed, 22, "{text}");
    }

    #[test]
    fn plugin_emit_writes_a_plugin_that_claude_validates() {
        let home = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let emit = Command::new(env!("CARGO_BIN_EXE_csr-engine"))
            .env("HOME", home.path())
            .args(["companion", "plugin", "emit"])
            .arg(out.path())
            .output()
            .unwrap();
        assert!(
            emit.status.success(),
            "{}",
            String::from_utf8_lossy(&emit.stderr)
        );
        let relay = std::fs::read_to_string(out.path().join("hooks/register.js")).unwrap();
        let engine = std::path::absolute(env!("CARGO_BIN_EXE_csr-engine")).unwrap();
        assert!(
            relay.contains(&format!("const ENGINE = '{}'", engine.display())),
            "{relay}"
        );
        let sock_dir = home.path().join(".claude-self-reflect/csr-companion/run");
        if sock_dir.as_os_str().len() + 14 <= 90 {
            assert!(relay.contains(&format!("const SOCK_DIR = '{}'", sock_dir.display())));
        } else {
            assert!(relay.contains("const SOCK_DIR = '/tmp/csr-"));
        }
        let Ok(found) = Command::new("claude")
            .env("HOME", home.path())
            .arg("--version")
            .output()
        else {
            eprintln!("claude is not on PATH: validate skipped");
            return;
        };
        if !found.status.success() {
            return;
        }
        let validated = Command::new("claude")
            .env("HOME", home.path())
            .env_remove("CLAUDE_CONFIG_DIR")
            .args(["plugin", "validate"])
            .arg(out.path())
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&validated.stdout);
        assert!(
            validated.status.success() && text.contains("Validation passed"),
            "{text}"
        );
    }
}

#[test]
fn png_screen_respects_requested_cell_dimensions() {
    for (cols, rows) in [(38, 30), (60, 40)] {
        let (_home, paths) = bare_home();
        let output = tempfile::tempdir().unwrap();
        let mut shot = args("main", cols, rows);
        shot.png = Some(output.path().into());
        shot.sid = Some("sess-now".into());
        shot.now = Some(1792320000.);
        csr_engine::companion::png::export(paths.clone(), &shot).unwrap();
        let actual = fs::read_to_string(output.path().join("screen.txt")).unwrap();
        let mut pane = Pane::new(paths, &shot);
        let expected = pane.frame(cols, rows, 1792320000.).join("\n") + "\n";
        assert_eq!(actual, expected, "{cols}x{rows}");
        assert_eq!(actual.lines().count(), rows);
        for line in actual.lines() {
            assert_eq!(
                csr_engine::companion::render::text::len(
                    &csr_engine::companion::render::text::plain(line)
                ),
                cols
            );
        }
    }
}

/// A fake herdr for the pane's event thread: it acknowledges the subscription and keeps it open,
/// and answers a snapshot with `snapshot`. Returns the socket and a way to push event lines.
#[cfg(unix)]
fn fake_herdr(socket: &std::path::Path, snapshot: &'static str) -> std::sync::mpsc::Sender<String> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    let listener = UnixListener::bind(socket).unwrap();
    let (push, pushed) = std::sync::mpsc::channel::<String>();
    let pushed = std::sync::Arc::new(std::sync::Mutex::new(pushed));
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let pushed = pushed.clone();
            std::thread::spawn(move || {
                let mut request = String::new();
                BufReader::new(conn.try_clone().unwrap())
                    .read_line(&mut request)
                    .unwrap();
                let request: serde_json::Value = serde_json::from_str(&request).unwrap();
                let mut conn = conn;
                if request["method"] == "events.subscribe" {
                    let _ = writeln!(
                        conn,
                        r#"{{"id":"csr","result":{{"type":"subscription_started"}}}}"#
                    );
                    while let Ok(line) = pushed.lock().unwrap().recv() {
                        if writeln!(conn, "{line}").is_err() {
                            break;
                        }
                    }
                } else {
                    let _ = writeln!(conn, r#"{{"id":"s","result":{{"snapshot":{snapshot}}}}}"#);
                }
            });
        }
    });
    push
}

/// herdr's default focus is one tab, but every attached client has a place of its own, and herdr
/// does not say where. A pane whose tab is not the default focus may be in front of someone: a
/// wake from the relay draws it at once, as it does with no herdr at all.
#[cfg(unix)]
#[test]
fn a_pane_outside_herdrs_default_focus_still_draws_when_woken() {
    use std::time::{Duration, Instant};
    let (_home, paths) = home();
    let socket = paths.home.join("herdr.sock");
    let _push = fake_herdr(
        &socket,
        r#"{"focused_tab_id":"w1:t1","panes":[{"pane_id":"w1:p2","tab_id":"w1:t1"},{"pane_id":"w1:p3","tab_id":"w1:t2"}]}"#,
    );
    let frame_log = paths.home.join("frames.log");
    let mut child = Command::new(env!("CARGO_BIN_EXE_csr-engine"))
        .env("HOME", &paths.home)
        .env("TERM_PROGRAM", "kitty")
        .env("CSR_PANE_FRAME_LOG", &frame_log)
        .env("CSR_PANE_HERDR_EVENTS", "1")
        .env("HERDR_SOCKET_PATH", &socket)
        .env("HERDR_PANE_ID", "w1:p3")
        .args([
            "companion",
            "pane",
            "--repo",
            env!("CARGO_MANIFEST_DIR"),
            "--watch",
            "--sid",
            "sess-now",
            "--agent",
            "w1:p2",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let frames = || fs::read_to_string(&frame_log).map_or(0, |t| t.lines().count());
    let wakes = paths
        .keep
        .join(format!("panes/sess-now.{}.wake", child.id()));
    let start = Instant::now();
    while (frames() == 0 || !wakes.exists()) && start.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(frames() >= 1, "the first frame");
    // Let the first frames and the snapshot settle, and the debounce's spacing pass.
    std::thread::sleep(Duration::from_millis(2500));
    let before = frames();
    assert!(csr_engine::companion::wake::notify(
        &paths.keep,
        "sess-now",
        csr_engine::companion::wake::Kind::Now
    ));
    let start = Instant::now();
    while frames() == before && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    assert!(
        frames() > before,
        "a woken pane in a tab that is not herdr's default focus drew nothing"
    );
}

/// Two panes of one session (a second terminal opened on it) each keep a wake pipe of their own:
/// a wake reaches both, and when one ends the other still hears every wake.
#[cfg(unix)]
#[test]
fn two_panes_of_one_session_both_draw_when_woken_and_one_ending_leaves_the_other_woken() {
    use std::time::{Duration, Instant};
    let (_home, paths) = home();
    let spawn = |log: &std::path::Path| {
        Command::new(env!("CARGO_BIN_EXE_csr-engine"))
            .env("HOME", &paths.home)
            .env("TERM_PROGRAM", "kitty")
            .env("CSR_PANE_FRAME_LOG", log)
            .args([
                "companion",
                "pane",
                "--repo",
                env!("CARGO_MANIFEST_DIR"),
                "--watch",
                "--sid",
                "sess-now",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap()
    };
    let first_log = paths.home.join("first.log");
    let second_log = paths.home.join("second.log");
    // A failed assertion must not leave a pane running.
    struct Reap(std::process::Child);
    impl Drop for Reap {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut first = Reap(spawn(&first_log));
    let second = Reap(spawn(&second_log));
    let (first_id, second_id) = (first.0.id(), second.0.id());
    let frames = |log: &std::path::Path| fs::read_to_string(log).map_or(0, |t| t.lines().count());
    let pipe = |pid: u32| paths.keep.join(format!("panes/sess-now.{pid}.wake"));
    let start = Instant::now();
    while (frames(&first_log) == 0
        || frames(&second_log) == 0
        || !pipe(first_id).exists()
        || !pipe(second_id).exists())
        && start.elapsed() < Duration::from_secs(60)
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    let both_listening = pipe(first_id).exists() && pipe(second_id).exists();
    // Let the first frames settle and the debounce's spacing pass.
    std::thread::sleep(Duration::from_millis(2500));
    let wake = |before: (usize, usize)| {
        assert!(csr_engine::companion::wake::notify(
            &paths.keep,
            "sess-now",
            csr_engine::companion::wake::Kind::Now
        ));
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(10)
            && (frames(&first_log) == before.0 || frames(&second_log) == before.1)
        {
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    let before = (frames(&first_log), frames(&second_log));
    wake(before);
    let after_both = (frames(&first_log), frames(&second_log));
    let _ = first.0.kill();
    let _ = first.0.wait();
    std::thread::sleep(Duration::from_millis(2500));
    let lone = frames(&second_log);
    assert!(csr_engine::companion::wake::notify(
        &paths.keep,
        "sess-now",
        csr_engine::companion::wake::Kind::Now
    ));
    let start = Instant::now();
    while frames(&second_log) == lone && start.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(20));
    }
    let after_lone = frames(&second_log);
    drop(second);
    assert!(both_listening, "each pane keeps a wake pipe of its own");
    assert!(
        after_both.0 > before.0 && after_both.1 > before.1,
        "a wake must reach both panes: before {before:?}, after {after_both:?}"
    );
    assert!(
        after_lone > lone,
        "the second pane lost its wakes when the first ended"
    );
}

/// herdr's events, opt-in: with a fake herdr on a socket, the watching pane subscribes, and when
/// herdr says the agent pane closed the pane ends by itself, cleanly, and removes its pid file.
#[cfg(unix)]
#[test]
fn the_pane_ends_when_herdr_says_the_agent_pane_closed() {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    };
    use std::time::{Duration, Instant};
    let (_home, paths) = home();
    let socket = paths.home.join("herdr.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let (subscribed, closed) = mpsc::channel::<()>();
    let (close, closing) = mpsc::channel::<()>();
    let seen = Arc::new(AtomicBool::new(false));
    let served = seen.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let mut request = String::new();
            BufReader::new(conn.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            let request: serde_json::Value = serde_json::from_str(&request).unwrap();
            let mut conn = conn;
            if request["method"] == "events.subscribe" {
                let listed: Vec<_> = request["params"]["subscriptions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|s| s["type"].as_str().unwrap().to_string())
                    .collect();
                served.store(
                    listed.contains(&"pane.closed".to_string()),
                    Ordering::SeqCst,
                );
                writeln!(
                    conn,
                    r#"{{"id":"csr","result":{{"type":"subscription_started"}}}}"#
                )
                .unwrap();
                subscribed.send(()).unwrap();
                if closing.recv_timeout(Duration::from_secs(20)).is_ok() {
                    let _ = writeln!(
                        conn,
                        r#"{{"event":"pane_closed","data":{{"pane_id":"w1:p2"}}}}"#
                    );
                }
            } else {
                // A snapshot that has this pane's tab on screen.
                let _ = writeln!(
                    conn,
                    r#"{{"id":"s","result":{{"snapshot":{{"focused_tab_id":"w1:t1","panes":[{{"pane_id":"w1:p2","tab_id":"w1:t1"}},{{"pane_id":"w1:p3","tab_id":"w1:t1"}}]}}}}}}"#
                );
            }
        }
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_csr-engine"))
        .env("HOME", &paths.home)
        .env("TERM_PROGRAM", "kitty")
        .env("CSR_PANE_HERDR_EVENTS", "1")
        .env("HERDR_SOCKET_PATH", &socket)
        .env("HERDR_PANE_ID", "w1:p3")
        .args([
            "companion",
            "pane",
            "--repo",
            env!("CARGO_MANIFEST_DIR"),
            "--watch",
            "--sid",
            "sess-now",
            "--agent",
            "w1:p2",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let pid = paths.keep.join("panes/sess-now.json");
    assert!(
        closed.recv_timeout(Duration::from_secs(10)).is_ok(),
        "the pane subscribed"
    );
    assert!(seen.load(Ordering::SeqCst), "and asked for pane.closed");
    assert!(pid.exists());
    assert!(child.try_wait().unwrap().is_none(), "it runs until told");
    close.send(()).unwrap();
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            panic!("the pane did not end when the agent pane closed");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success(), "{status:?}");
    assert!(!pid.exists(), "its pid file is gone");
}
