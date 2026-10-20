use super::{
    cli::PaneArgs,
    pane::Pane,
    paths::{self, Paths},
    private,
    render::{
        kitty::{self, Drawn},
        text::{self, wrap},
    },
};
use crate::process::CommandDeadline;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers, MouseButton,
        MouseEventKind,
    },
    execute, terminal,
};
use serde_json::json;
use std::{
    fs::{self, FileTimes, OpenOptions},
    io::{self, IsTerminal, Write},
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt},
        process::CommandExt,
    },
    panic::{catch_unwind, AssertUnwindSafe},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Condvar, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime},
};
thread_local! {
    static PANIC_LOCATION:  std::cell::RefCell<Option<String>> = const {
        std::cell::RefCell::new(None)
    };
}
static ENDING: AtomicBool = AtomicBool::new(false);
extern "C" fn ending(_: libc::c_int) {
    ENDING.store(true, Ordering::Relaxed);
}

pub struct TerminalGuard {
    raw: bool,
}
impl TerminalGuard {
    pub fn enter() -> io::Result<Self> {
        let raw = io::stdin().is_terminal();
        let guard = Self { raw };
        guard.resume()?;
        Ok(guard)
    }
    fn resume(&self) -> io::Result<()> {
        if self.raw {
            terminal::enable_raw_mode()?;
            execute!(io::stdout(), EnableMouseCapture)?;
        }
        print!("\x1b]0;CSR\x07\x1b[?25l\x1b]11;rgb:fb/fb/f9\x1b\\");
        io::stdout().flush()
    }
    pub fn restore() {
        let _ = terminal::disable_raw_mode();
        let mut out = io::stdout();
        let _ = execute!(out, DisableMouseCapture);
        let _ = writeln!(
            out,
            "{}\x1b]111\x1b\\\x1b[?25h\x1b]0;\x07",
            kitty::DELETE_ALL
        );
        let _ = out.flush();
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if self.raw {
            let _ = terminal::disable_raw_mode();
        }
        Self::restore();
    }
}
struct Signals {
    old: [libc::sighandler_t; 2],
}
impl Signals {
    fn install() -> Self {
        ENDING.store(false, Ordering::Relaxed);
        Self {
            old: unsafe {
                [
                    libc::signal(libc::SIGTERM, ending as *const () as libc::sighandler_t),
                    libc::signal(libc::SIGHUP, ending as *const () as libc::sighandler_t),
                ]
            },
        }
    }
}
impl Drop for Signals {
    fn drop(&mut self) {
        unsafe {
            libc::signal(libc::SIGTERM, self.old[0]);
            libc::signal(libc::SIGHUP, self.old[1]);
        }
    }
}

struct BeatState {
    seen: Instant,
    stopped: bool,
}
pub struct Heartbeat {
    state: Arc<(Mutex<BeatState>, Condvar)>,
    thread: Option<JoinHandle<()>>,
}
impl Heartbeat {
    pub fn start(path: Option<PathBuf>, every: Duration, stall: Duration) -> Self {
        let state = Arc::new((
            Mutex::new(BeatState {
                seen: Instant::now(),
                stopped: false,
            }),
            Condvar::new(),
        ));
        let shared = state.clone();
        let thread = path.map(|path| {
            thread::spawn(move || {
                let (lock, wake) = &*shared;
                let mut state = lock.lock().unwrap();
                loop {
                    let (next, _) = wake.wait_timeout(state, every).unwrap();
                    state = next;
                    if state.stopped {
                        break;
                    }
                    if state.seen.elapsed() > stall {
                        continue;
                    }
                    drop(state);
                    if let Ok(file) = OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_NOFOLLOW)
                        .open(&path)
                    {
                        let _ = file.set_times(
                            FileTimes::new()
                                .set_accessed(SystemTime::now())
                                .set_modified(SystemTime::now()),
                        );
                    }
                    state = lock.lock().unwrap();
                }
            })
        });
        Self { state, thread }
    }
    pub fn alive(&self) {
        if let Ok(mut s) = self.state.0.lock() {
            s.seen = Instant::now();
        }
    }
    pub fn stop(&mut self) {
        if let Ok(mut s) = self.state.0.lock() {
            s.stopped = true;
            self.state.1.notify_all();
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
impl Drop for Heartbeat {
    fn drop(&mut self) {
        self.stop();
    }
}
struct PidFile {
    path: Option<PathBuf>,
}
impl PidFile {
    fn new(paths: &Paths, sid: &str) -> io::Result<Self> {
        let path = paths.kept_file("panes", sid);
        if let Some(p) = &path {
            private::write_atomic(
                p,
                &json!({
                    "pid": std::process::id(),
                })
                .to_string(),
                &paths.keep,
            )?;
        }
        Ok(Self { path })
    }
}
impl Drop for PidFile {
    fn drop(&mut self) {
        if let Some(p) = &self.path {
            if paths::read_json(p)["pid"] == std::process::id() {
                let _ = fs::remove_file(p);
            }
        }
    }
}
pub fn log_error(paths: &Paths, text: &str) {
    let path = paths.keep.join("pane-errors.log");
    if fs::metadata(&path).is_ok_and(|m| m.len() >= 1 << 20) {
        let _ = fs::rename(&path, paths.keep.join("pane-errors.log.1"));
    }
    let text = crate::redact::redact_private(text);
    let _ = private::append(
        &path,
        &format!("{}{}", text, if text.ends_with('\n') { "" } else { "\n" }),
        &paths.keep,
    );
}
pub struct Boundary {
    pub failures: usize,
    pub due: f64,
    pub last: String,
}
impl Default for Boundary {
    fn default() -> Self {
        Self {
            failures: 0,
            due: 0.,
            last: String::new(),
        }
    }
}
impl Boundary {
    pub fn ready(&self, now: f64) -> bool {
        self.failures == 0 || now >= self.due
    }
    pub fn run<T>(&mut self, paths: &Paths, now: f64, call: impl FnOnce() -> T) -> Option<T> {
        match catch_unwind(AssertUnwindSafe(call)) {
            Ok(v) => {
                self.failures = 0;
                self.last.clear();
                Some(v)
            }
            Err(p) => {
                self.failures += 1;
                let wait = [1, 2, 5, 10, 30][(self.failures - 1).min(4)];
                self.due = now + wait as f64;
                self.last = p
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_else(|| "frame panicked".into());
                if let Some(location) = PANIC_LOCATION.with(|p| p.borrow_mut().take()) {
                    self.last.push_str(&format!(" · {location}"));
                }
                log_error(
                    paths,
                    &format!(
                        "{} pid {} failure {}: {}",
                        chrono::Local::now().format("%Y-%m-%dT%H:%M:%S"),
                        std::process::id(),
                        self.failures,
                        self.last
                    ),
                );
                None
            }
        }
    }
    pub fn card(&self, cols: usize, now: f64) -> String {
        let wait = (self.due - now).ceil().max(0.) as u64;
        wrap(
            &format!(
                "CSR pane error · {} · retry in {wait}s",
                text::one_line(&self.last, 200)
            ),
            cols.saturating_sub(2).max(1),
            4,
        )
        .into_iter()
        .enumerate()
        .map(|(i, s)| {
            format!(
                "\x1b[{};1H\x1b[48;2;200;68;42m\x1b[38;2;238;245;250m\x1b[1m {s}\x1b[K\x1b[0m",
                i + 1
            )
        })
        .collect()
    }
}
#[cfg(test)]
fn mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).ok()?.modified().ok()
}
pub fn stopping() -> bool {
    ENDING.load(Ordering::Relaxed)
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Stamp {
    size: u64,
    modified: SystemTime,
    identity: (u64, u64),
}
fn stamp(path: &Path) -> io::Result<Stamp> {
    let m = fs::metadata(path)?;
    if !m.is_file() {
        return Err(io::Error::other("engine is not a file"));
    }
    Ok(Stamp {
        size: m.len(),
        modified: m.modified()?,
        identity: (m.dev(), m.ino()),
    })
}
struct Reload {
    loaded: Option<Stamp>,
    observed: Option<Stamp>,
    since: Instant,
    attempted: bool,
}
impl Reload {
    fn new(path: &Path, now: Instant) -> Self {
        let loaded = stamp(path).ok();
        Self {
            loaded,
            observed: loaded,
            since: now,
            attempted: false,
        }
    }
    /// The binary on disk is again the one this process runs, so an earlier
    /// reload failure no longer applies.
    fn settled(&self) -> bool {
        self.observed.is_some() && self.observed == self.loaded
    }
    fn poll(&mut self, path: &Path, now: Instant, force: bool) -> io::Result<bool> {
        let current = stamp(path);
        let observed = current.as_ref().ok().copied();
        if observed != self.observed {
            self.observed = observed;
            self.since = now;
            self.attempted = false;
        }
        let current = match current {
            Ok(value) => value,
            Err(error) if !self.attempted => {
                self.attempted = true;
                return Err(error);
            }
            Err(_) => return Ok(false),
        };
        if self.attempted
            || (!force && Some(current) == self.loaded)
            || now.saturating_duration_since(self.since) < Duration::from_secs(1)
        {
            return Ok(false);
        }
        self.attempted = true;
        let out = std::process::Command::new(path)
            .args(["companion", "--version"])
            .output_with_deadline(Duration::from_secs(5))?;
        if stamp(path)? != current {
            return Err(io::Error::other("engine changed during probe"));
        }
        let named = out.stdout.starts_with(b"csr-engine-companion ");
        if !out.status.success() || !named {
            return Err(io::Error::other("engine version probe failed"));
        }
        Ok(true)
    }
}
fn failed_exec(
    exec: impl FnOnce() -> io::Error,
    resume: impl FnOnce() -> io::Result<()>,
) -> String {
    let error = exec();
    let mut message = format!("engine reload: {error}");
    if let Err(error) = resume() {
        message.push_str(&format!("; terminal: {error}"));
    }
    message
}
fn recover_terminal(
    epoch: &AtomicUsize,
    seen: &mut usize,
    drawn: &mut Option<Drawn>,
    resume: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let current = epoch.load(Ordering::Acquire);
    if current != *seen {
        *drawn = None;
        resume()?;
        *seen = current;
    }
    Ok(())
}
pub fn watch(paths: Paths, args: &PaneArgs) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let mut reload = Reload::new(&exe, Instant::now());
    let mut reload_error = None;
    let epoch = Arc::new(AtomicUsize::new(0));
    let panics = epoch.clone();
    let mut restored = 0;
    let mut pane = Pane::new(paths.clone(), args);
    pane.herdr = super::phone::Herdr::from_env(&paths.home);
    let mine = PidFile::new(&paths, args.sid.as_deref().unwrap_or(""))?;
    let mut beat = Heartbeat::start(
        mine.path.clone(),
        Duration::from_secs(5),
        Duration::from_secs(120),
    );
    let signals = Signals::install();
    let terminal = TerminalGuard::enter()?;
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(location) = info.location() {
            PANIC_LOCATION.with(|p| {
                *p.borrow_mut() = Some(format!(
                    "{}:{}",
                    Path::new(location.file())
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy(),
                    location.line()
                ))
            });
        }
        TerminalGuard::restore();
        panics.fetch_add(1, Ordering::Release);
    }));
    let mut drawn = None;
    let mut boundary = Boundary::default();
    let mut pressed = true;
    let mut last = Instant::now();
    let mut last_key = None;
    let mut restart_due = None;
    let result = (|| -> anyhow::Result<()> {
        while !ENDING.load(Ordering::Relaxed) {
            beat.alive();
            let now = paths::now();
            recover_terminal(&epoch, &mut restored, &mut drawn, || terminal.resume())?;
            if boundary.failures >= 3 {
                restart_due.get_or_insert_with(|| Instant::now() + Duration::from_secs(30));
            } else {
                restart_due = None;
            }
            let force = restart_due.is_some_and(|due| Instant::now() >= due);
            match reload.poll(&exe, Instant::now(), force) {
                Ok(true) => {
                    if stopping() {
                        break;
                    }
                    TerminalGuard::restore();
                    reload_error = Some(failed_exec(
                        || {
                            std::process::Command::new(&exe)
                                .args(std::env::args_os().skip(1))
                                .exec()
                        },
                        || terminal.resume(),
                    ));
                    drawn = None;
                }
                Err(error) => reload_error = Some(format!("engine reload: {error}")),
                Ok(false) if reload.settled() => reload_error = None,
                Ok(false) => (),
            }
            // The Shift Map's hold is up and it folds: draw it now.
            pressed |= pane.map_wait(now).1;
            // A compaction ended: draw it now.
            pressed |= pane.compaction_gone();
            if (boundary.failures > 0 && (boundary.ready(now) || pressed))
                || (boundary.failures == 0 && (pressed || last.elapsed() >= Duration::from_secs(1)))
            {
                let (cols, rows) = terminal::size().unwrap_or((60, 30));
                let frame = boundary.run(&paths, now, || {
                    pane.frame(cols as usize, rows as usize, now)
                });
                recover_terminal(&epoch, &mut restored, &mut drawn, || terminal.resume())?;
                if let Some(written) = frame {
                    if drawn.is_none() {
                        print!("\x1b]0;CSR\x07\x1b[?25l\x1b]11;rgb:fb/fb/f9\x1b\\");
                    }
                    if terminal.raw {
                        terminal::enable_raw_mode()?;
                        execute!(io::stdout(), EnableMouseCapture)?;
                    }
                    print!(
                        "{}",
                        kitty::repaint_pictures(
                            drawn.as_ref(),
                            cols as usize,
                            rows as usize,
                            &written,
                            &pane.placed
                        )
                    );
                    drawn = Some(Drawn {
                        cols: cols as usize,
                        rows: rows as usize,
                        written,
                        placed: pane.placed.clone(),
                    });
                } else {
                    drawn = None;
                    print!("{}", boundary.card(cols as usize, now));
                }
                if let Some(error) = &reload_error {
                    let card = Boundary {
                        last: error.clone(),
                        due: now,
                        failures: 0,
                    };
                    print!("{}", card.card(cols as usize, now));
                    drawn = None;
                }
                io::stdout().flush()?;
                pressed = false;
                last = Instant::now();
            }
            recover_terminal(&epoch, &mut restored, &mut drawn, || terminal.resume())?;
            if terminal.raw && event::poll(Duration::from_millis(200))? {
                recover_terminal(&epoch, &mut restored, &mut drawn, || terminal.resume())?;
                match event::read()? {
                    Event::Resize(_, _) => {
                        pressed = true;
                    }
                    Event::Mouse(m) => match m.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            if !pane.click(m.row as usize, m.column as usize, now) {
                                break;
                            }
                            pressed = true;
                        }
                        MouseEventKind::ScrollUp => {
                            pane.scroll(-3);
                            pressed = true;
                        }
                        MouseEventKind::ScrollDown => {
                            pane.scroll(3);
                            pressed = true;
                        }
                        _ => (),
                    },
                    Event::Key(k) if k.kind != event::KeyEventKind::Release => {
                        if k.modifiers.contains(KeyModifiers::CONTROL)
                            && k.code == KeyCode::Char('c')
                        {
                            ENDING.store(true, Ordering::Relaxed);
                            break;
                        }
                        let key = match k.code {
                            KeyCode::Char(c) => c.to_lowercase().to_string(),
                            KeyCode::Esc => "escape".into(),
                            KeyCode::Up => "up".into(),
                            KeyCode::Down => "down".into(),
                            _ => String::new(),
                        };
                        let typing =
                            last_key.is_some_and(|t: Instant| t.elapsed() < Duration::from_secs(1));
                        last_key = Some(Instant::now());
                        if !(key == "q" && typing) && !pane.key(&key) {
                            ENDING.store(true, Ordering::Relaxed);
                            break;
                        }
                        pressed = true;
                    }
                    _ => (),
                }
            } else if !terminal.raw {
                thread::sleep(Duration::from_millis(200));
            }
        }
        Ok(())
    })();
    beat.stop();
    std::panic::set_hook(hook);
    drop(terminal);
    drop(signals);
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    #[test]
    fn test_a_frame_that_raises_draws_the_card_and_the_loop_goes_on() {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::new(d.path().into());
        let mut b = Boundary::default();
        assert!(b.run(&p, 1000., || panic!("broken")).is_none());
        assert!(b
            .card(100, 1000.)
            .contains("CSR pane error · broken · retry in 1s"));
        assert!(!b.ready(1000.5));
        assert_eq!(b.run(&p, 1001., || 7), Some(7));
        assert_eq!(b.failures, 0);
    }
    #[test]
    fn test_retries_wait_out_the_backoff() {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::new(d.path().into());
        let mut b = Boundary::default();
        for (i, wait) in [1., 2., 5., 10., 30., 30.].iter().enumerate() {
            b.run(&p, 1000., || panic!("bad"));
            assert_eq!(b.due, 1000. + wait);
            assert_eq!(b.failures, i + 1);
        }
    }
    #[test]
    fn test_a_narrow_pane_wraps_the_card_over_its_top_rows() {
        let b = Boundary {
            last: "broken frame at pane.rs:12".into(),
            failures: 1,
            due: 5.,
        };
        let card = b.card(12, 0.);
        assert_eq!(card.matches("H\x1b[48;2").count(), 4);
    }
    #[test]
    fn test_the_log_moves_aside_at_one_megabyte_and_starts_again() {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::new(d.path().into());
        log_error(&p, "first");
        let log = p.keep.join("pane-errors.log");
        File::options()
            .append(true)
            .open(&log)
            .unwrap()
            .set_len(1 << 20)
            .unwrap();
        log_error(&p, "after token=abc123");
        assert_eq!(
            fs::metadata(p.keep.join("pane-errors.log.1"))
                .unwrap()
                .len(),
            1 << 20
        );
        assert_eq!(fs::read_to_string(log).unwrap(), "after token=[redacted]\n");
    }
    #[test]
    fn test_the_heartbeat_stops_when_the_loop_stalls_and_when_stopped() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("pid");
        File::create(&path).unwrap();
        let mut beat = Heartbeat::start(
            Some(path.clone()),
            Duration::from_millis(10),
            Duration::from_millis(60),
        );
        thread::sleep(Duration::from_millis(120));
        let old = mtime(&path);
        thread::sleep(Duration::from_millis(30));
        assert_eq!(mtime(&path), old);
        beat.alive();
        thread::sleep(Duration::from_millis(30));
        assert_ne!(mtime(&path), old);
        beat.stop();
        let old = mtime(&path);
        thread::sleep(Duration::from_millis(30));
        assert_eq!(mtime(&path), old);
    }
}

#[cfg(test)]
mod reload_regressions {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    fn script(path: &Path, text: &str) {
        fs::write(path, text).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    #[test]
    fn reload_requires_an_existing_stable_binary_and_successful_version_probe() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("engine");
        let now = Instant::now();
        let mut reload = Reload::new(&path, now);
        assert!(reload.poll(&path, now, false).is_err());
        script(&path, "#!/bin/sh\nexit 1\n");
        assert!(!reload.poll(&path, now, false).unwrap());
        assert!(!reload
            .poll(&path, now + Duration::from_millis(999), false)
            .unwrap());
        assert!(reload
            .poll(&path, now + Duration::from_secs(1), false)
            .is_err());
        assert!(!reload
            .poll(&path, now + Duration::from_secs(2), false)
            .unwrap());
        script(&path, "#!/bin/sh\nprintf 'csr-engine-companion 1.0\\n'\n");
        assert!(!reload
            .poll(&path, now + Duration::from_secs(3), false)
            .unwrap());
        assert!(reload
            .poll(&path, now + Duration::from_secs(4), false)
            .unwrap());
    }
    #[test]
    fn a_probe_that_is_not_the_companion_is_not_executed() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("engine");
        let now = Instant::now();
        let mut reload = Reload::new(&path, now);
        script(&path, "#!/bin/sh\nexit 0\n");
        assert!(!reload.poll(&path, now, false).unwrap());
        assert!(reload
            .poll(&path, now + Duration::from_secs(1), false)
            .is_err());
    }
    #[test]
    fn a_reload_error_clears_once_the_running_binary_is_back() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("engine");
        let aside = d.path().join("aside");
        script(&path, "#!/bin/sh\nprintf 'csr-engine-companion 1.0\\n'\n");
        let now = Instant::now();
        let mut reload = Reload::new(&path, now);
        assert!(reload.settled());
        fs::rename(&path, &aside).unwrap();
        assert!(reload.poll(&path, now, false).is_err());
        assert!(!reload.settled());
        fs::rename(&aside, &path).unwrap();
        assert!(!reload
            .poll(&path, now + Duration::from_secs(2), false)
            .unwrap());
        assert!(reload.settled());
    }
    #[test]
    fn a_binary_that_changes_during_its_probe_is_not_executed() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("engine");
        let now = Instant::now();
        let mut reload = Reload::new(&path, now);
        script(&path, "#!/bin/sh\nprintf 'changed' >> \"$0\"\nexit 0\n");
        assert!(!reload.poll(&path, now, false).unwrap());
        assert!(reload
            .poll(&path, now + Duration::from_secs(1), false)
            .is_err());
    }
    #[test]
    fn panic_recovery_rearms_before_the_error_card_and_invalidates_other_thread_frames() {
        let epoch = Arc::new(AtomicUsize::new(0));
        let worker = epoch.clone();
        std::thread::spawn(move || worker.fetch_add(1, Ordering::Release))
            .join()
            .unwrap();
        let mut seen = 0;
        let mut drawn = Some(Drawn {
            cols: 1,
            rows: 1,
            written: vec!["old".into()],
            placed: vec![],
        });
        let mut resumed = false;
        recover_terminal(&epoch, &mut seen, &mut drawn, || {
            resumed = true;
            Ok(())
        })
        .unwrap();
        assert!(
            resumed,
            "raw mode and mouse capture must precede cards and input"
        );
        assert!(
            drawn.is_none(),
            "another thread's panic requires a full repaint"
        );
        assert_eq!(seen, 1);
    }
}

#[cfg(test)]
mod failed_exec_tests {
    use super::*;
    #[test]
    fn failed_exec_resumes_the_terminal_and_keeps_heartbeat_and_pid_ownership() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().into());
        let mine = PidFile::new(&paths, "s").unwrap();
        let path = mine.path.as_ref().unwrap().clone();
        let before = mtime(&path);
        let mut beat = Heartbeat::start(
            Some(path.clone()),
            Duration::from_millis(5),
            Duration::from_secs(1),
        );
        let mut resumed = false;
        let error = failed_exec(
            || io::Error::other("exec refused"),
            || {
                resumed = true;
                Ok(())
            },
        );
        assert!(resumed);
        assert!(error.contains("exec refused"));
        std::thread::sleep(Duration::from_millis(30));
        assert_ne!(mtime(&path), before);
        beat.stop();
        drop(mine);
        assert!(!path.exists());
    }
}

#[cfg(test)]
mod probe_timeout_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn a_version_probe_that_hangs_is_stopped_after_five_seconds() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("engine");
        let now = Instant::now();
        let mut reload = Reload::new(&path, now);
        fs::write(&path, "#!/bin/sh\nexec /bin/sleep 10\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(!reload.poll(&path, now, false).unwrap());
        let started = Instant::now();
        let error = reload
            .poll(&path, now + Duration::from_secs(1), false)
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(8));
    }
}
