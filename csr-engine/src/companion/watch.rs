use super::{
    cli::PaneArgs,
    herdr_events::Watcher as HerdrEvents,
    pane::Pane,
    paths::{self, Paths},
    private,
    render::{
        kitty::{self, Drawn},
        text::{self, wrap},
    },
    send,
    wake::{self, Debounce, Kind as WakeKind},
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
/// Counts the loop's turns, for the watchdog to see that it is going round.
static TICKS: AtomicUsize = AtomicUsize::new(0);
extern "C" fn ending(_: libc::c_int) {
    ENDING.store(true, Ordering::Relaxed);
}

/// The terminal on stdin hung up: its emulator or multiplexer is gone and the kernel says so
/// with POLLHUP. Nothing else tells a pane: the terminal's reads return end of file, which
/// crossterm's reader takes for "nothing yet" and retries at once, so a pane that is inside its
/// `poll` when this happens never comes back out of it (and never sees SIGTERM's flag). So every
/// call into crossterm is made only after this has said no. A stdin that was never a terminal
/// is never gone. One that was a terminal at the start (`TTY`) is gone once the kernel says
/// POLLHUP or POLLERR, or once it is no longer one at all: on Linux the ioctls of a hung-up
/// terminal fail, and `isatty` with them.
fn tty_gone() -> bool {
    lost(
        TTY.load(Ordering::Relaxed),
        // An interrupted ioctl is no hangup.
        unsafe {
            libc::isatty(libc::STDIN_FILENO) == 1
                || io::Error::last_os_error().raw_os_error() == Some(libc::EINTR)
        },
        || wait_on(libc::STDIN_FILENO, Duration::ZERO),
    )
}
/// Whether stdin was a terminal when the pane started.
static TTY: AtomicBool = AtomicBool::new(false);
fn lost(was_terminal: bool, is_terminal: bool, look: impl FnOnce() -> Input) -> bool {
    was_terminal && (!is_terminal || look() == Input::Gone)
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Input {
    Quiet,
    Ready,
    Gone,
}
/// Wait up to `wait` for the terminal `fd` to have bytes for us, or to hang up.
fn wait_on(fd: libc::c_int, wait: Duration) -> Input {
    let mut tty = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let millis = wait.as_millis().min(i32::MAX as u128) as libc::c_int;
    // An interrupted wait (a signal came) is a quiet one: the loop looks at its flag next.
    if unsafe { libc::poll(&mut tty, 1, millis) } <= 0 {
        return Input::Quiet;
    }
    if tty.revents & libc::POLLNVAL != 0 {
        // The descriptor cannot be polled (macOS says so of some live terminals): it is no
        // evidence of anything, and poll would answer again at once. Sleep the wait out.
        thread::sleep(wait);
        Input::Quiet
    } else if tty.revents & (libc::POLLHUP | libc::POLLERR) != 0 {
        Input::Gone
    } else if tty.revents & libc::POLLIN != 0 {
        Input::Ready
    } else {
        Input::Quiet
    }
}
/// How long a wait for input goes before crossterm is asked about what it holds already (a
/// resize arrives by a signal, not on the terminal) and the terminal is looked at again.
const LOOK: Duration = Duration::from_millis(100);
/// Crossterm's events, up to `wait`, with the terminal looked at before every call into it.
fn next_input(wait: Duration) -> io::Result<Input> {
    let start = Instant::now();
    loop {
        TICKS.fetch_add(1, Ordering::Relaxed);
        if tty_gone() {
            return Ok(Input::Gone);
        }
        // Events crossterm parsed already, and a resize, wait in it and not on the terminal.
        if event::poll(Duration::ZERO)? {
            return Ok(Input::Ready);
        }
        let left = wait.saturating_sub(start.elapsed());
        if left.is_zero() || ENDING.load(Ordering::Relaxed) {
            return Ok(Input::Quiet);
        }
        match wait_on(libc::STDIN_FILENO, left.min(LOOK)) {
            Input::Gone => return Ok(Input::Gone),
            Input::Ready if event::poll(Duration::ZERO)? => return Ok(Input::Ready),
            _ => (),
        }
    }
}

pub struct TerminalGuard {
    raw: bool,
}
impl TerminalGuard {
    pub fn enter() -> io::Result<Self> {
        let raw = io::stdin().is_terminal();
        TTY.store(raw, Ordering::Relaxed);
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
        // A terminal that is gone has nothing to restore, and a write to it would only fail.
        if tty_gone() {
            return;
        }
        let _ = terminal::disable_raw_mode();
        let mut out = io::stdout();
        let _ = out.write_all(&Self::giveback());
        let _ = out.flush();
    }
    /// What the terminal is sent to be as it was found.
    fn giveback() -> Vec<u8> {
        let mut bytes = Vec::new();
        let _ = execute!(bytes, DisableMouseCapture);
        let _ = writeln!(
            bytes,
            "{}\x1b]111\x1b\\\x1b[?25h\x1b]0;\x07",
            kitty::DELETE_ALL
        );
        bytes
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if self.raw && !tty_gone() {
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
                &json!({ "pid": std::process::id() }).to_string(),
                &paths.keep,
            )?;
        }
        Ok(Self { path })
    }
}
impl Drop for PidFile {
    fn drop(&mut self) {
        if let Some(p) = &self.path {
            remove_pid_file(p);
        }
    }
}
/// Remove the pid file at `path`, if it is this process's.
fn remove_pid_file(path: &Path) {
    if paths::read_json(path)["pid"] == std::process::id() {
        let _ = fs::remove_file(path);
    }
}

/// How long the loop may go without turning, with the terminal gone or an end asked for, before
/// the watchdog ends the pane.
const STUCK: Duration = Duration::from_secs(2);
/// The last resort of a pane whose loop cannot end it: something inside the loop never returns
/// (crossterm's reader, on a terminal that hung up after the loop looked, spins on end of file
/// and never comes back). A thread of its own sees that the loop has not turned for `STUCK`
/// while the terminal is gone or an end was asked for, leaves what a clean exit leaves (no
/// pipe, lock or pid file; the terminal given back if it is there) and exits. A pane that is
/// only busy, with a live terminal and no end asked for, is never touched.
struct Watchdog {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Watchdog {
    fn start(wake: Option<wake::Cleaner>, pid_file: Option<PathBuf>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let ended = stop.clone();
        let thread = thread::spawn(move || {
            let mut turns = TICKS.load(Ordering::Relaxed);
            let mut still = Instant::now();
            while !ended.load(Ordering::Relaxed) {
                thread::park_timeout(Duration::from_millis(250));
                let now = TICKS.load(Ordering::Relaxed);
                if now != turns {
                    (turns, still) = (now, Instant::now());
                    continue;
                }
                let asked = ENDING.load(Ordering::Relaxed);
                if still.elapsed() < STUCK || ended.load(Ordering::Relaxed) {
                    continue;
                }
                let gone = tty_gone();
                if !asked && !gone {
                    continue;
                }
                if let Some(wake) = &wake {
                    wake.remove();
                }
                if let Some(path) = &pid_file {
                    remove_pid_file(path);
                }
                if !gone {
                    // The loop may hold the terminal's lock: no waiting on it for long.
                    unsafe { libc::alarm(1) };
                    let _ = terminal::disable_raw_mode();
                    let bytes = TerminalGuard::giveback();
                    unsafe {
                        libc::write(libc::STDOUT_FILENO, bytes.as_ptr().cast(), bytes.len());
                    }
                }
                unsafe { libc::_exit(if asked { 0 } else { 1 }) };
            }
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }
    fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}
impl Drop for Watchdog {
    fn drop(&mut self) {
        self.stop();
    }
}
/// One line per left click: where it landed and whether a popup was up after it.
fn log_click(paths: &Paths, row: &serde_json::Value) {
    let path = paths.keep.join("pane-clicks.log");
    if fs::metadata(&path).is_ok_and(|m| m.len() >= 1 << 18) {
        let _ = fs::rename(&path, paths.keep.join("pane-clicks.log.1"));
    }
    let _ = private::append(&path, &format!("{row}\n"), &paths.keep);
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
/// The file that points the pane at a development build: one line, the path of a csr-engine
/// binary. While it names an executable file the pane runs that binary and reloads on every
/// rebuild; once it is gone the pane goes back to the engine it was started with.
pub const ENGINE_OVERRIDE: &str = "pane-engine";
/// The engine a pane was started with, carried across an exec into an override build so that
/// removing the override can bring the pane home.
const HOME_ENGINE: &str = "CSR_PANE_HOME_ENGINE";
/// Which binary the pane should be running: the override when it names a file, else home.
fn engine_target(keep: &Path, home: &Path) -> PathBuf {
    fs::read_to_string(keep.join(ENGINE_OVERRIDE))
        .ok()
        .map(|text| PathBuf::from(text.trim()))
        .filter(|path| path.is_absolute() && path.is_file())
        .unwrap_or_else(|| home.to_path_buf())
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
    /// Watch a different binary than the one this process runs: the first stable, probed look
    /// at it reloads.
    fn switch(path: &Path, now: Instant) -> Self {
        Self {
            loaded: None,
            observed: stamp(path).ok(),
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
/// How often a pane asks herdr which session its agent pane runs.
const FOLLOW: Duration = Duration::from_secs(5);
/// The pane's own arguments with `--sid` set to `sid`: any given one dropped, the new one last.
fn with_sid(args: &[std::ffi::OsString], sid: &str) -> Vec<std::ffi::OsString> {
    let mut out = Vec::with_capacity(args.len() + 2);
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "--sid" {
            it.next();
        } else if !arg.to_str().is_some_and(|a| a.starts_with("--sid=")) {
            out.push(arg.clone());
        }
    }
    out.extend(["--sid".into(), sid.into()]);
    out
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
/// After a panic anywhere (the hook deleted every picture): the terminal is armed again and the
/// next draw is a full repaint. True when that happened, and then the pane has to draw at once
/// whatever its gates say, or it stays blank until its next deadline.
fn recover_terminal(
    epoch: &AtomicUsize,
    seen: &mut usize,
    drawn: &mut Option<Drawn>,
    resume: impl FnOnce() -> io::Result<()>,
) -> io::Result<bool> {
    let current = epoch.load(Ordering::Acquire);
    if current == *seen {
        return Ok(false);
    }
    *drawn = None;
    resume()?;
    *seen = current;
    Ok(true)
}
pub fn watch(paths: Paths, args: &PaneArgs) -> anyhow::Result<()> {
    let running = std::env::current_exe()?;
    let home = std::env::var_os(HOME_ENGINE)
        .map(PathBuf::from)
        .unwrap_or_else(|| running.clone());
    let mut exe = engine_target(&paths.keep, &home);
    let mut reload = if exe == running {
        Reload::new(&exe, Instant::now())
    } else {
        Reload::switch(&exe, Instant::now())
    };
    let mut looked = Instant::now();
    let mut followed = Instant::now();
    let mut reload_error = None;
    let epoch = Arc::new(AtomicUsize::new(0));
    let panics = epoch.clone();
    let mut restored = 0;
    let mut pane = Pane::new(paths.clone(), args);
    pane.enable_cards();
    pane.herdr = super::phone::Herdr::from_env(&paths.home);
    let signals = Signals::install();
    let sid = args.sid.as_deref().unwrap_or("");
    let mine = PidFile::new(&paths, sid)?;
    // The relay's wakes arrive here, where no pid is trusted: see `wake.rs`.
    let mut wake_pipe = wake::Listener::open(&paths.keep, sid);
    let mut beat = Heartbeat::start(
        mine.path.clone(),
        Duration::from_secs(5),
        Duration::from_secs(120),
    );
    let terminal = TerminalGuard::enter()?;
    let mut watchdog = Watchdog::start(
        wake_pipe.as_ref().and_then(wake::Listener::cleaner),
        mine.path.clone(),
    );
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
    // The pane's pictures and the bands last cut from them.
    let mut cut: (Vec<kitty::Placement>, Vec<kitty::Placement>) = (vec![], vec![]);
    let mut boundary = Boundary::default();
    let mut pressed = true;
    let mut last = Instant::now();
    let mut last_key = None;
    let mut restart_due = None;
    let mut wake = Debounce::default();
    // For tests that count what the loop draws: one line per frame drawn and written out, and why.
    let frame_log = std::env::var_os("CSR_PANE_FRAME_LOG").map(PathBuf::from);
    // On inside herdr (`CSR_PANE_HERDR_EVENTS=0` turns it off): herdr says when the agent's status
    // changes and when its pane ends. Without herdr, or when it cannot be reached, nothing changes.
    let events = HerdrEvents::from_env(args.agent.clone());
    // For tests of the watchdog: make the loop stick, at once (`busy`) or where it would end
    // (`ending`: the terminal is gone or an end was asked for), as a hang inside a call would.
    let stick = std::env::var("CSR_PANE_TEST_STICK").ok();
    let result = (|| -> anyhow::Result<()> {
        while !ENDING.load(Ordering::Relaxed) {
            if stick.as_deref() == Some("busy") {
                loop {
                    thread::sleep(Duration::from_secs(1));
                }
            }
            // Before any draw: a write to a hung-up terminal is an error `print!` panics on.
            if terminal.raw && tty_gone() {
                break;
            }
            beat.alive();
            TICKS.fetch_add(1, Ordering::Relaxed);
            let now = paths::now();
            pressed |= recover_terminal(&epoch, &mut restored, &mut drawn, || terminal.resume())?;
            if boundary.failures >= 3 {
                restart_due.get_or_insert_with(|| Instant::now() + Duration::from_secs(30));
            } else {
                restart_due = None;
            }
            let force = restart_due.is_some_and(|due| Instant::now() >= due);
            if looked.elapsed() >= Duration::from_secs(1) {
                looked = Instant::now();
                let target = engine_target(&paths.keep, &home);
                if target != exe {
                    reload = if target == running {
                        Reload::new(&target, looked)
                    } else {
                        Reload::switch(&target, looked)
                    };
                    exe = target;
                }
            }
            let reloading = reload_error.clone();
            match reload.poll(&exe, Instant::now(), force) {
                Ok(true) => {
                    if stopping() {
                        break;
                    }
                    // The watchdog's alarm outlives an exec: it stands down before one, and is
                    // armed again if the exec fails.
                    watchdog.stop();
                    // For tests: an alarm armed as the watchdog's is, to see it does not outlive the exec.
                    if std::env::var_os("CSR_PANE_TEST_ALARM").is_some() {
                        unsafe { libc::alarm(1) };
                    }
                    TerminalGuard::restore();
                    reload_error = Some(failed_exec(
                        || {
                            unsafe { libc::alarm(0) };
                            std::process::Command::new(&exe)
                                .args(std::env::args_os().skip(1))
                                .env(HOME_ENGINE, &home)
                                .exec()
                        },
                        || terminal.resume(),
                    ));
                    watchdog = Watchdog::start(
                        wake_pipe.as_ref().and_then(wake::Listener::cleaner),
                        mine.path.clone(),
                    );
                    drawn = None;
                }
                Err(error) => reload_error = Some(format!("engine reload: {error}")),
                Ok(false) if reload.settled() => reload_error = None,
                Ok(false) => (),
            }
            // A `/clear` or a resume gave the agent pane a new session id, and nothing tells
            // the pane: it draws the old session until it starts again on the new one. herdr
            // follows the id, so ask it, and exec with the new `--sid` as an engine reload does.
            if let Some(agent) = args
                .agent
                .as_deref()
                .filter(|_| followed.elapsed() >= FOLLOW)
            {
                followed = Instant::now();
                if let Some(next) = send::current_session(&paths, agent).filter(|s| s != sid) {
                    if stopping() {
                        break;
                    }
                    watchdog.stop();
                    TerminalGuard::restore();
                    let argv: Vec<_> = std::env::args_os().skip(1).collect();
                    reload_error = Some(failed_exec(
                        || {
                            unsafe { libc::alarm(0) };
                            std::process::Command::new(&running)
                                .args(with_sid(&argv, &next))
                                .env(HOME_ENGINE, &home)
                                .exec()
                        },
                        || terminal.resume(),
                    ));
                    watchdog = Watchdog::start(
                        wake_pipe.as_ref().and_then(wake::Listener::cleaner),
                        mine.path.clone(),
                    );
                    drawn = None;
                }
            }
            // The error card comes and goes with the reload's error: draw when it does.
            pressed |= reload_error != reloading;
            if events.as_ref().is_some_and(HerdrEvents::agent_gone) {
                break;
            }
            // The relay says the session did something: draw when the debounce lets it.
            let (now_wake, mut soon_wake) = wake_pipe
                .as_mut()
                .map_or((false, false), wake::Listener::take);
            soon_wake |= events.as_ref().is_some_and(HerdrEvents::take_status);
            if now_wake || soon_wake {
                let at = Instant::now();
                if now_wake {
                    wake.signal(at, WakeKind::Now);
                }
                if soon_wake {
                    wake.signal(at, WakeKind::Burst);
                }
            }
            pressed |= wake.ready(Instant::now());
            // The Shift Map's hold is up and it folds: draw it now.
            pressed |= pane.map_wait(now).1;
            // A compaction ended: draw it now.
            pressed |= pane.compaction_gone();
            if (boundary.failures > 0 && (boundary.ready(now) || pressed))
                || (boundary.failures == 0
                    && (pressed || (last.elapsed() >= Duration::from_secs(1) && pane.due(now))))
            {
                let (cols, rows) = terminal::size().unwrap_or((60, 30));
                let why = if pressed { "pressed" } else { "due" };
                let frame = boundary.run(&paths, now, || {
                    pane.frame(cols as usize, rows as usize, now)
                });
                // This draw is a full repaint already, and it clears `pressed`.
                recover_terminal(&epoch, &mut restored, &mut drawn, || terminal.resume())?;
                if let Some(written) = frame {
                    if drawn.is_none() {
                        print!("\x1b]0;CSR\x07\x1b[?25l\x1b]11;rgb:fb/fb/f9\x1b\\");
                    }
                    // Only on a full repaint: any byte, even an unchanged mode, makes herdr place every picture again.
                    if terminal.raw && drawn.is_none() {
                        terminal::enable_raw_mode()?;
                        execute!(io::stdout(), EnableMouseCapture)?;
                    }
                    if cut.0 != pane.placed {
                        cut = (pane.placed.clone(), kitty::bands(&pane.placed));
                    }
                    print!(
                        "{}",
                        kitty::repaint_pictures(
                            drawn.as_ref(),
                            cols as usize,
                            rows as usize,
                            &written,
                            &cut.1
                        )
                    );
                    let ids = kitty::picture_ids(
                        drawn.as_ref(),
                        cols as usize,
                        rows as usize,
                        &written,
                        &cut.1,
                    );
                    drawn = Some(Drawn {
                        cols: cols as usize,
                        rows: rows as usize,
                        written,
                        placed: cut.1.clone(),
                        ids,
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
                if let Some(log) = &frame_log {
                    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(log)
                    {
                        let _ = writeln!(file, "{why}");
                    }
                }
                pressed = false;
                last = Instant::now();
                wake.drew(last);
            }
            pressed |= recover_terminal(&epoch, &mut restored, &mut drawn, || terminal.resume())?;
            // A wake that is waiting for its gathering or its spacing shortens the poll.
            let wait = wake.next().map_or(Duration::from_millis(200), |at| {
                at.saturating_duration_since(Instant::now())
                    .clamp(Duration::from_millis(5), Duration::from_millis(200))
            });
            let input = if terminal.raw {
                next_input(wait)?
            } else {
                thread::sleep(wait);
                Input::Quiet
            };
            if input == Input::Gone {
                break;
            }
            if input == Input::Ready {
                pressed |=
                    recover_terminal(&epoch, &mut restored, &mut drawn, || terminal.resume())?;
                match event::read()? {
                    Event::Resize(_, _) => {
                        pressed = true;
                    }
                    Event::Mouse(m) => match m.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            let mut row = pane.click_target(m.row as usize, m.column as usize);
                            let kept = pane.click(m.row as usize, m.column as usize, now);
                            row["at"] = json!(now);
                            row["after"] = pane.click_target(m.row as usize, m.column as usize)
                                ["popup"]
                                .clone();
                            log_click(&paths, &row);
                            if !kept {
                                break;
                            }
                            // A tap on the popup's "c copy" puts the text out as the key does.
                            if let Some(text) = pane.take_copy() {
                                copy_to_clipboard(&text);
                            }
                            pressed = true;
                        }
                        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                            let mark = pane.ui_mark();
                            pane.scroll(if m.kind == MouseEventKind::ScrollUp {
                                -3
                            } else {
                                3
                            });
                            pressed |= pane.ui_mark() != mark;
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
                            KeyCode::Tab if k.modifiers.contains(KeyModifiers::SHIFT) => {
                                "shift+tab".into()
                            }
                            KeyCode::Tab => "tab".into(),
                            KeyCode::BackTab => "shift+tab".into(),
                            KeyCode::Up => "up".into(),
                            KeyCode::Down => "down".into(),
                            KeyCode::Left => "left".into(),
                            KeyCode::Right => "right".into(),
                            _ => String::new(),
                        };
                        let typing =
                            last_key.is_some_and(|t: Instant| t.elapsed() < Duration::from_secs(1));
                        last_key = Some(Instant::now());
                        // A key the pane does not answer leaves the page as it was: no frame.
                        let mark = pane.ui_mark();
                        if !(key == "q" && typing) && !pane.key(&key) {
                            ENDING.store(true, Ordering::Relaxed);
                            break;
                        }
                        if let Some(text) = pane.take_copy() {
                            copy_to_clipboard(&text);
                            pressed = true;
                        }
                        pressed |= pane.ui_mark() != mark;
                    }
                    _ => (),
                }
            }
        }
        if stick.is_some() {
            loop {
                thread::sleep(Duration::from_secs(1));
            }
        }
        Ok(())
    })();
    watchdog.stop();
    beat.stop();
    std::panic::set_hook(hook);
    drop(terminal);
    drop(signals);
    result
}
/// Put `text` on the clipboard: an OSC 52 sequence for terminals that pass it on, and `pbcopy`
/// on a Mac, where herdr may not. Best effort; a failure is silent.
fn copy_to_clipboard(text: &str) {
    use base64::Engine;
    print!(
        "\x1b]52;c;{}\x07",
        base64::engine::general_purpose::STANDARD.encode(text)
    );
    let _ = io::stdout().flush();
    if cfg!(target_os = "macos") {
        if let Ok(mut child) = std::process::Command::new("pbcopy")
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
        }
    }
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
    fn test_a_new_session_replaces_the_sid_and_keeps_every_other_argument() {
        let args = |list: &[&str]| {
            list.iter()
                .map(|a| a.into())
                .collect::<Vec<std::ffi::OsString>>()
        };
        for (given, want) in [
            (
                &[
                    "companion",
                    "pane",
                    "--watch",
                    "--sid",
                    "old",
                    "--agent",
                    "w1:p1",
                ][..],
                &[
                    "companion",
                    "pane",
                    "--watch",
                    "--agent",
                    "w1:p1",
                    "--sid",
                    "new",
                ][..],
            ),
            (
                &["companion", "pane", "--sid=old", "--watch"][..],
                &["companion", "pane", "--watch", "--sid", "new"][..],
            ),
            (
                &["companion", "pane", "--watch"][..],
                &["companion", "pane", "--watch", "--sid", "new"][..],
            ),
        ] {
            assert_eq!(with_sid(&args(given), "new"), args(want), "{given:?}");
        }
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
    fn test_a_terminal_that_stops_being_one_is_gone_and_a_stdin_that_never_was_is_not() {
        let gone = || Input::Gone;
        let quiet = || Input::Quiet;
        // Linux: the hangup makes isatty fail.
        assert!(lost(true, false, quiet));
        // macOS: isatty holds and poll says hangup.
        assert!(lost(true, true, gone));
        // A live terminal.
        assert!(!lost(true, true, quiet));
        // Redirected input never counts, whatever poll would say of a closed pipe.
        assert!(!lost(false, false, gone));
        assert!(!lost(false, true, gone));
    }
    #[test]
    fn test_a_descriptor_poll_cannot_watch_is_slept_on_not_spun_on() {
        // A descriptor number no process has open: poll answers POLLNVAL at once.
        let start = Instant::now();
        assert_eq!(wait_on(900_000, Duration::from_millis(150)), Input::Quiet);
        assert!(
            start.elapsed() >= Duration::from_millis(140),
            "returned after {:?}: a busy wait",
            start.elapsed()
        );
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
    fn the_override_file_points_the_pane_at_a_build_and_its_removal_brings_it_home() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().join("installed");
        let dev = d.path().join("dev");
        script(&dev, "#!/bin/sh\nprintf 'csr-engine-companion 1.0\\n'\n");
        assert_eq!(engine_target(d.path(), &home), home);
        // A relative path, or one that is not a file, is ignored.
        fs::write(d.path().join(ENGINE_OVERRIDE), "dev\n").unwrap();
        assert_eq!(engine_target(d.path(), &home), home);
        fs::write(
            d.path().join(ENGINE_OVERRIDE),
            format!("{}\n", d.path().display()),
        )
        .unwrap();
        assert_eq!(engine_target(d.path(), &home), home);
        fs::write(
            d.path().join(ENGINE_OVERRIDE),
            format!("{}\n", dev.display()),
        )
        .unwrap();
        assert_eq!(engine_target(d.path(), &home), dev);
        fs::remove_file(d.path().join(ENGINE_OVERRIDE)).unwrap();
        assert_eq!(engine_target(d.path(), &home), home);
    }
    #[test]
    fn switching_to_another_binary_reloads_once_it_is_stable_and_probed() {
        let d = tempfile::tempdir().unwrap();
        let dev = d.path().join("dev");
        script(&dev, "#!/bin/sh\nprintf 'csr-engine-companion 1.0\\n'\n");
        let now = Instant::now();
        let mut reload = Reload::switch(&dev, now);
        assert!(!reload.settled());
        assert!(!reload.poll(&dev, now, false).unwrap());
        assert!(reload
            .poll(&dev, now + Duration::from_secs(1), false)
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
            ids: vec![],
        });
        let mut resumed = false;
        let recovered = recover_terminal(&epoch, &mut seen, &mut drawn, || {
            resumed = true;
            Ok(())
        })
        .unwrap();
        assert!(
            recovered,
            "a recovery is a reason to draw, past both gates: the pictures are gone"
        );
        assert!(
            !recover_terminal(&epoch, &mut seen, &mut drawn, || panic!("only once")).unwrap(),
            "and only once"
        );
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
