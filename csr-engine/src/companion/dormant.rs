//! A pane with nothing to show for a while rests as a small waiting process.
//!
//! After `CSR_PANE_IDLE_MINS` minutes (5 unless set, "0.5" is 30 s; 0 or `CSR_PANE_NO_SLEEP=1` off)
//! with no wake from the relay, no key, click or resize, no change in its own session's
//! transcript and an agent that is not working, the pane takes its pictures down, prints a card
//! in words, and `exec`s itself with `--dormant`: same pid, so the wake pipe's name, the pid file
//! and herdr's split all stay. The new image holds no fonts, no database and no pictures and
//! draws nothing; this module imports nothing from `render` or `pane` so that stays true. It
//! waits in one `poll` on the terminal, the wake pipe, herdr's event socket and a pipe the
//! signals write to, and every five seconds touches its pid file and looks at its transcript.
//! Any wake `exec`s back into the ordinary `--watch` with the arguments the pane started with.
//!
//! A pane whose agent pane is still there but runs no session any more (Claude exited, the shell
//! stayed) has nothing to wait for: it ends instead of resting, once herdr has said so on every
//! look for ten minutes running (a failed look says nothing either way).
//!
//! Nothing that waited is lost on the way in: the transcript's stamp at the idle decision goes
//! with the exec, a copy of the wake pipe's read end outlives it, and the resting pane looks at
//! both before it first waits. Outside herdr the terminal's own focus reports (`CSI ? 1004`) are
//! the only word on whether anyone looks: a pane rests only after a focus-out and no focus-in
//! since, and a focus-in wakes it.
use super::{
    cli::PaneArgs,
    herdr_events,
    paths::Paths,
    send,
    sleep_log::{Asked, Decision, Journal, Probe},
    wake,
    watch::{self, PidFile},
};
use crossterm::{
    event::{
        DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture, Event,
    },
    execute, terminal,
};
use serde_json::{json, Value};
use std::{
    ffi::OsString,
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, RawFd},
        unix::{net::UnixStream, process::CommandExt},
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicI32, Ordering},
    time::{Duration, Instant, SystemTime},
};

/// The flag that turns the rest on in the exec.
pub const FLAG: &str = "--dormant";
/// The idle minutes when nothing says otherwise.
pub const DEFAULT_MINS: f64 = 5.;
/// How long a pane that was kept awake (focused, or its agent working) waits to be asked again.
pub const ASK_AGAIN: Duration = Duration::from_secs(60);
/// How often the resting process touches its pid file and looks at its transcript.
pub const BEAT: Duration = Duration::from_secs(5);
/// Where a resting pane is told which transcript to watch.
const TRANSCRIPT: &str = "CSR_PANE_REST_TRANSCRIPT";
/// And what that transcript looked like when the pane decided to rest.
const MARK: &str = "CSR_PANE_REST_MARK";
/// And the copy of the wake pipe's read end the pane hands over.
const WAKE_FD: &str = "CSR_PANE_REST_WAKE_FD";
/// How long herdr has to say, look after look, that an agent pane runs no session before the
/// pane that belongs to it ends.
pub const ABSENT_FOR: Duration = Duration::from_secs(600);
/// And how many of those looks it takes.
pub const ABSENT_LOOKS: u32 = 3;
/// A look this old no longer counts as "still": the pane is not ended on a stale word.
pub const ABSENT_FRESH: Duration = Duration::from_secs(30);

/// How long a pane idles before it rests: None when resting is off.
pub fn idle_limit(var: impl Fn(&str) -> Option<String>) -> Option<Duration> {
    if var("CSR_PANE_NO_SLEEP").as_deref() == Some("1") {
        return None;
    }
    let mins = match var("CSR_PANE_IDLE_MINS") {
        None => DEFAULT_MINS,
        Some(text) => (text.trim().parse::<f64>().ok())
            .filter(|m| m.is_finite() && *m >= 0. && *m < 1e6)
            .unwrap_or(DEFAULT_MINS),
    };
    (mins > 0.).then(|| Duration::from_secs_f64(mins * 60.))
}

/// How far into a transcript the pane has read: a byte offset, at the start of a line.
pub type Mark = u64;
/// A transcript as it is now: its length.
pub fn mark(path: &Path) -> Option<Mark> {
    Some(std::fs::metadata(path).ok()?.len())
}

/// The most a look reads in one batch, and so the longest line that is judged by its content.
const BATCH: usize = 1 << 20;
/// How many batches one look reads at most; the rest waits for the next look.
const BATCHES_PER_LOOK: usize = 16;
/// Which file a transcript is: its device and inode.
pub type Ident = (u64, u64);

/// What a transcript did since the pane last read it. Claude Code touches every session's
/// transcript now and then (Remote Control's `bridge-session` and `session-owner` lines, a
/// rewritten mtime): that is not someone at the session, and only a turn is.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Activity {
    /// Nothing new: no growth (a changed mtime alone counts for nothing), or half a line.
    Quiet,
    /// Whole lines came and none of them is a turn; the kind of the first.
    Ignored(String),
    /// A turn: an assistant line, or a user line that is not meta; or a single line too long
    /// to judge.
    Real(&'static str),
    /// The first look at a file: what is there is history.
    Rebaselined,
    /// The file is another one (a new inode) or shorter than what was read: what is in it was
    /// not judged, and a missed wake is worse than a spare one.
    Replaced,
}
impl Activity {
    /// Whether this keeps a pane awake or wakes it.
    pub fn is_real(&self) -> bool {
        matches!(self, Self::Real(_) | Self::Replaced)
    }
    /// For the journal: why this look did or did not count.
    pub fn cause(&self) -> Option<String> {
        match self {
            Self::Quiet => None,
            Self::Ignored(kind) => Some(format!("transcript ignored: {kind}")),
            Self::Real(kind) => Some(format!("transcript: {kind}")),
            Self::Rebaselined => Some("transcript rebaselined".into()),
            Self::Replaced => Some("transcript replaced".into()),
        }
    }
}
/// Whether one transcript line is a turn. A whitelist: the two kinds of line that are a turn say
/// so, every other kind (and anything unreadable) is not.
pub fn judge_line(line: &[u8]) -> Result<&'static str, String> {
    let Ok(value) = serde_json::from_slice::<Value>(line) else {
        return Err("unparseable".into());
    };
    match value["type"].as_str() {
        Some("assistant") => Ok("assistant"),
        Some("user") if value["isMeta"] != true && value["isSidechain"] != true => Ok("user"),
        Some(kind) => Err(kind.to_owned()),
        None => Err("untyped".into()),
    }
}
/// The lines that came after `offset`, judged in batches of at most 1 MiB; `offset` moves past
/// the last whole line read, and `ident` is the file it was read from. Only a single line longer
/// than a batch is taken for a turn without being read.
pub fn transcript_activity(path: &Path, offset: &mut u64, ident: &mut Option<Ident>) -> Activity {
    use std::io::Seek;
    use std::os::unix::fs::MetadataExt;
    let Ok(mut file) = std::fs::File::open(path) else {
        return Activity::Quiet;
    };
    let Ok(meta) = file.metadata() else {
        return Activity::Quiet;
    };
    let (len, now) = (meta.len(), (meta.dev(), meta.ino()));
    let replaced = ident.is_some_and(|was| was != now) || len < *offset;
    *ident = Some(now);
    if replaced {
        *offset = len;
        return Activity::Replaced;
    }
    let mut ignored = None;
    for _ in 0..BATCHES_PER_LOOK {
        let want = (len - *offset).min(BATCH as u64) as usize;
        if want == 0 {
            break;
        }
        let mut bytes = vec![0u8; want];
        if file.seek(io::SeekFrom::Start(*offset)).is_err() || file.read_exact(&mut bytes).is_err()
        {
            break;
        }
        // Only whole lines count; half of one is judged when the rest comes.
        let Some(end) = bytes.iter().rposition(|b| *b == b'\n') else {
            if want == BATCH {
                *offset += want as u64;
                return Activity::Real("oversized line");
            }
            break;
        };
        *offset += end as u64 + 1;
        for line in bytes[..end]
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
        {
            match judge_line(line) {
                Ok(kind) => return Activity::Real(kind),
                Err(kind) => {
                    ignored.get_or_insert(kind);
                }
            }
        }
    }
    ignored.map_or(Activity::Quiet, Activity::Ignored)
}
/// A transcript being followed: the path, which file that was, and how far it has been read.
#[derive(Default, Debug)]
pub struct Tail {
    path: Option<PathBuf>,
    offset: Option<Mark>,
    ident: Option<Ident>,
}
impl Tail {
    /// Following `path` from `offset` (None: from the file's end when first looked at).
    pub fn at(path: Option<PathBuf>, offset: Option<Mark>) -> Self {
        Self {
            path,
            offset,
            ident: None,
        }
    }
    /// How far it has been read, if it has been looked at.
    pub fn offset(&self) -> Option<Mark> {
        self.offset
    }
    pub fn look(&mut self, path: Option<&Path>) -> Activity {
        use std::os::unix::fs::MetadataExt;
        let Some(path) = path else {
            return Activity::Quiet;
        };
        if self.path.as_deref() != Some(path) {
            self.path = Some(path.to_path_buf());
            self.offset = None;
            self.ident = None;
        }
        let Some(mut offset) = self.offset else {
            // First look: what is there already is history.
            return match std::fs::metadata(path) {
                Ok(meta) => {
                    self.offset = Some(meta.len());
                    self.ident = Some((meta.dev(), meta.ino()));
                    Activity::Rebaselined
                }
                Err(_) => Activity::Quiet,
            };
        };
        let activity = transcript_activity(path, &mut offset, &mut self.ident);
        self.offset = Some(offset);
        activity
    }
}

/// What the loop saw since it last asked.
#[derive(Clone, Copy, Default, Debug)]
pub struct Seen {
    /// The relay's byte came through the pipe.
    pub wake: bool,
    /// A key, a click, a scroll or a resize.
    pub input: bool,
    /// herdr says the agent is working.
    pub working: bool,
}

/// When the pane last had something to do, and how long it may go without.
pub struct Idle {
    limit: Option<Duration>,
    since: Instant,
}
impl Idle {
    pub fn new(limit: Option<Duration>, now: Instant) -> Self {
        Self { limit, since: now }
    }
    pub fn observe(&mut self, now: Instant, seen: Seen) {
        if seen.wake || seen.input || seen.working {
            self.since = now;
        }
    }
    /// What the own transcript did since the last look: only a real turn is activity.
    pub fn transcript(&mut self, now: Instant, activity: &Activity) {
        if activity.is_real() {
            self.since = now;
        }
    }
    pub fn due(&self, now: Instant) -> bool {
        self.limit
            .is_some_and(|limit| now.saturating_duration_since(self.since) >= limit)
    }
    /// Not now: ask again in `wait`.
    pub fn defer(&mut self, now: Instant, wait: Duration) {
        if let Some(limit) = self.limit {
            self.since = (now + wait).checked_sub(limit).unwrap_or(now);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
    /// Nothing is happening: rest.
    Sleep,
    /// Someone is looking, or the agent is working, or nobody can say: stay up.
    Busy,
}
/// Whether the terminal says it is looked at, by its focus reports (`CSI I` in, `CSI O` out).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Focus {
    /// No report yet, or the terminal sends none.
    #[default]
    Unknown,
    In,
    Out,
}
impl Focus {
    /// For the journal.
    pub fn word(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::In => "in",
            Self::Out => "out",
        }
    }
    /// The focus a terminal event reports, if it does.
    pub fn of(event: &Event) -> Option<Self> {
        match event {
            Event::FocusGained => Some(Self::In),
            Event::FocusLost => Some(Self::Out),
            _ => None,
        }
    }
}
/// Whether a terminal event is someone at the pane. Looking away is not.
pub fn is_activity(event: &Event) -> bool {
    !matches!(event, Event::FocusLost)
}
/// The verdict from herdr's word on the agent's pane and on this one (each None when herdr
/// could not say), and the terminal's own focus. herdr's word stands when it has one; with none
/// (outside herdr, or herdr out of reach) only a terminal that reported focus-out may rest.
pub fn decide(agent: Option<&Value>, own: Option<&Value>, terminal: Focus) -> Verdict {
    decide_why(agent, own, terminal).0
}
/// The verdict, and the reason for it in words, for the journal.
pub fn decide_why(
    agent: Option<&Value>,
    own: Option<&Value>,
    terminal: Focus,
) -> (Verdict, &'static str) {
    let focused = |pane: Option<&Value>| pane.is_some_and(|p| p["focused"] == true);
    if focused(agent) {
        return (Verdict::Busy, "herdr says the agent pane is focused");
    }
    if focused(own) {
        return (Verdict::Busy, "herdr says this pane is focused");
    }
    if agent.is_some_and(|p| p["agent_status"] == "working") {
        return (Verdict::Busy, "herdr says the agent is working");
    }
    if agent.is_none() && own.is_none() {
        return if terminal == Focus::Out {
            (Verdict::Sleep, "no word from herdr, terminal focus is out")
        } else {
            (
                Verdict::Busy,
                "no word from herdr and the terminal has not looked away",
            )
        };
    }
    (Verdict::Sleep, "herdr says unfocused and not working")
}
/// What a look at herdr came to.
pub struct Look {
    pub verdict: Verdict,
    pub cause: &'static str,
    pub asked: Asked,
}
/// Ask herdr, then decide.
pub fn look(agent: Option<&str>, own: Option<&str>, terminal: Focus) -> Look {
    let pane = |id: Option<&str>| -> (Probe, Option<Value>) {
        let Some(id) = id else {
            return (Probe::NoId, None);
        };
        let Some(said) = send::herdr(&["pane", "get", id])
            .and_then(|said| serde_json::from_str::<Value>(&said).ok())
        else {
            return (Probe::Failed, None);
        };
        match Some(said["result"]["pane"].clone()).filter(Value::is_object) {
            Some(pane) => (Probe::Ok, Some(pane)),
            None => (Probe::Absent, None),
        }
    };
    let (agent_probe, agent_pane) = pane(agent);
    let (own_probe, own_pane) = pane(own);
    let (verdict, cause) = decide_why(agent_pane.as_ref(), own_pane.as_ref(), terminal);
    Look {
        verdict,
        cause,
        asked: Asked {
            agent: agent_probe,
            own: own_probe,
        },
    }
}

/// How long herdr has said that an agent pane runs no session.
#[derive(Default, Debug)]
pub struct Absence {
    since: Option<Instant>,
    last: Option<Instant>,
    looks: u32,
}
impl Absence {
    /// One look at the agent pane's session. A session resets it; a look that failed changes
    /// nothing, for it is no word either way.
    pub fn observe(&mut self, now: Instant, state: &send::Session) {
        match state {
            send::Session::Id(_) => *self = Self::default(),
            send::Session::Absent => {
                self.since.get_or_insert(now);
                self.last = Some(now);
                self.looks += 1;
            }
            send::Session::Unknown => (),
        }
    }
    /// An absence is being timed: do not rest on it, the looks stop when the pane does.
    pub fn pending(&self) -> bool {
        self.since.is_some()
    }
    /// Ten minutes of looks that said so, the latest of them a moment ago.
    pub fn confirmed(&self, now: Instant) -> bool {
        match (self.since, self.last) {
            (Some(since), Some(last)) => {
                self.looks >= ABSENT_LOOKS
                    && now.saturating_duration_since(since) >= ABSENT_FOR
                    && now.saturating_duration_since(last) <= ABSENT_FRESH
            }
            _ => false,
        }
    }
}

/// The resting loop's clock: a beat on a deadline that every return from the wait checks, so
/// that events arriving all the time cannot keep the beat from coming.
pub struct Beat {
    next: Instant,
}
impl Beat {
    pub fn new(now: Instant) -> Self {
        Self { next: now + BEAT }
    }
    /// The wait that ends at the deadline, in milliseconds for `poll`.
    pub fn wait_ms(&self, now: Instant) -> i32 {
        let left = self.next.saturating_duration_since(now);
        (left.as_millis() as i32).saturating_add(1)
    }
    /// True once per beat, whenever it is asked at or after the deadline.
    pub fn due(&mut self, now: Instant) -> bool {
        if now < self.next {
            return false;
        }
        self.next = now + BEAT;
        true
    }
}
/// When a socket that would not open is tried again: after 5 s, then 10, 20 and 30.
pub struct Backoff {
    next: Instant,
    wait: Duration,
}
impl Backoff {
    pub fn new(now: Instant) -> Self {
        Self {
            next: now + BEAT,
            wait: BEAT,
        }
    }
    pub fn due(&self, now: Instant) -> bool {
        now >= self.next
    }
    pub fn failed(&mut self, now: Instant) {
        self.wait = (self.wait * 2).min(Duration::from_secs(30));
        self.next = now + self.wait;
    }
}

/// The arguments the pane started with, resting: the flag last.
pub fn with_dormant(args: &[OsString]) -> Vec<OsString> {
    let mut out = without_dormant(args);
    out.push(FLAG.into());
    out
}
/// The arguments of the ordinary pane again.
pub fn without_dormant(args: &[OsString]) -> Vec<OsString> {
    args.iter().filter(|a| *a != FLAG).cloned().collect()
}
/// `args` made to carry `--sid sid`: unchanged when they do already.
pub fn carrying_sid(args: &[OsString], sid: &str) -> Vec<OsString> {
    let joined = format!("--sid={sid}");
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "--sid" && it.next().is_some_and(|v| v.to_str() == Some(sid)) {
            return args.to_vec();
        }
        if arg.to_str() == Some(joined.as_str()) {
            return args.to_vec();
        }
    }
    watch::with_sid(args, sid)
}

/// How old a quiet time is, in a few words.
pub fn age(secs: u64) -> String {
    match secs {
        0..=59 => "under a minute".into(),
        60..=5399 => format!("{} min", secs / 60),
        5400..=172_799 => format!("{} h", (secs + 1800) / 3600),
        _ => format!("{} d", (secs + 43_200) / 86_400),
    }
}
/// What the resting pane says, in words, wrapped to `cols` and placed at the top of a cleared
/// screen.
pub fn card(project: &str, quiet_for: Option<u64>, cols: usize) -> String {
    let mut text = String::from("CSR is resting.");
    if !project.is_empty() {
        text.push_str(&format!(" {project}"));
        match quiet_for {
            Some(secs) => text.push_str(&format!(", last turn {} ago.", age(secs))),
            None => text.push('.'),
        }
    }
    text.push_str(" Wakes on your next prompt, a key, or a click.");
    let mut out = String::from("\x1b[2J\x1b[H\x1b[?25l");
    for (row, line) in wrap(&text, cols.saturating_sub(2).max(10))
        .into_iter()
        .enumerate()
    {
        out.push_str(&format!("\x1b[{};2H{line}", row + 2));
    }
    out
}
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = vec![];
    for word in text.split_whitespace() {
        match lines.last_mut() {
            Some(line) if line.chars().count() + 1 + word.chars().count() <= width => {
                line.push(' ');
                line.push_str(word);
            }
            _ => lines.push(word.to_owned()),
        }
    }
    lines
}

/// What ends the rest.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cause {
    /// Back to the ordinary pane.
    Wake,
    /// End the process: the terminal or the agent is gone, or an end was asked for.
    Exit,
}
/// A line from herdr's event stream, in words for the journal.
pub fn describe(line: &Value) -> String {
    if line["error"].is_object() {
        return format!(
            "herdr error {}",
            line["error"]["code"].as_str().unwrap_or("without a code")
        );
    }
    format!(
        "herdr event {}",
        line["event"].as_str().unwrap_or("unnamed")
    )
}
/// What bytes from the terminal were, in words for the journal.
pub fn stdin_why(revents: i16, bytes: &[u8]) -> &'static str {
    let has = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
    if revents & libc::POLLNVAL != 0 {
        "terminal could not be polled"
    } else if revents & (libc::POLLHUP | libc::POLLERR) != 0 {
        "terminal hung up"
    } else if bytes.is_empty() {
        "terminal read came back empty"
    } else if has(b"\x1b[I") {
        "terminal focus-in"
    } else if has(b"\x1b[<") || has(b"\x1b[M") {
        "click or scroll"
    } else {
        "key"
    }
}
/// A line from herdr's event stream: what it means for a resting pane.
pub fn judge(
    line: &Value,
    agent: Option<&str>,
    own_pane: Option<&str>,
    own_tab: Option<&str>,
) -> Option<Cause> {
    if line["error"].is_object() {
        return Some(Cause::Wake);
    }
    let name = line["event"].as_str()?.replace('.', "_");
    let data = &line["data"];
    let pane = data["pane_id"].as_str();
    let mine = |id: Option<&str>| id.is_some() && (id == agent || id == own_pane);
    match name.as_str() {
        "pane_agent_status_changed" if pane.is_some() && pane == agent => {
            (data["agent_status"] == "working").then_some(Cause::Wake)
        }
        "pane_focused" if mine(pane) => Some(Cause::Wake),
        "tab_focused"
            if data["tab_id"].as_str().is_some() && data["tab_id"].as_str() == own_tab =>
        {
            Some(Cause::Wake)
        }
        "pane_closed" | "pane_exited" if pane.is_some() && pane == agent => Some(Cause::Exit),
        _ => None,
    }
}
/// The subscription a resting pane makes.
pub fn subscription(agent: Option<&str>) -> String {
    let mut types = vec![
        json!({ "type": "pane.focused" }),
        json!({ "type": "tab.focused" }),
        json!({ "type": "pane.closed" }),
        json!({ "type": "pane.exited" }),
    ];
    if let Some(agent) = agent {
        types.push(json!({ "type": "pane.agent_status_changed", "pane_id": agent }));
    }
    json!({ "id": "csr-rest", "method": "events.subscribe", "params": { "subscriptions": types } })
        .to_string()
}
/// What the terminal's poll bits mean, and `bytes` (read only when there are some to read).
pub fn stdin_cause(revents: i16, bytes: impl FnOnce() -> Vec<u8>) -> Option<Cause> {
    if revents & (libc::POLLHUP | libc::POLLERR) != 0 {
        Some(Cause::Exit)
    } else if revents & libc::POLLNVAL != 0 {
        // A terminal macOS cannot poll is no evidence of rest: go back to the ordinary pane.
        Some(Cause::Wake)
    } else if revents & libc::POLLIN != 0 {
        typed(&bytes())
    } else {
        None
    }
}
/// What bytes from the terminal mean to a resting pane: a key, a click, a focus-in or a scroll
/// wakes it, and so does a read that came back empty; "the window lost focus" (`ESC [ O`) does
/// not.
pub fn typed(bytes: &[u8]) -> Option<Cause> {
    const OUT: &[u8] = b"\x1b[O";
    let mut rest = bytes;
    let mut other = bytes.is_empty();
    while !rest.is_empty() {
        if rest.starts_with(OUT) {
            rest = &rest[OUT.len()..];
        } else {
            other = true;
            break;
        }
    }
    other.then_some(Cause::Wake)
}
/// What the terminal holds, up to a few hundred bytes.
fn read_stdin() -> Vec<u8> {
    let mut bytes = [0u8; 256];
    let n = unsafe { libc::read(libc::STDIN_FILENO, bytes.as_mut_ptr().cast(), bytes.len()) };
    bytes[..n.max(0) as usize].to_vec()
}

static SIGNAL_FD: AtomicI32 = AtomicI32::new(-1);
extern "C" fn on_signal(sig: libc::c_int) {
    let fd = SIGNAL_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        let byte = sig as u8;
        unsafe { libc::write(fd, (&byte as *const u8).cast(), 1) };
    }
}
/// A pipe the signal handler writes to, so a resize or an end is just another readable fd.
fn signal_pipe() -> Option<RawFd> {
    let mut fds = [0 as libc::c_int; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return None;
    }
    for fd in fds {
        unsafe {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK);
        }
    }
    SIGNAL_FD.store(fds[1], Ordering::Relaxed);
    for sig in [libc::SIGWINCH, libc::SIGTERM, libc::SIGHUP] {
        unsafe { libc::signal(sig, on_signal as *const () as libc::sighandler_t) };
    }
    Some(fds[0])
}
fn signal_cause(fd: RawFd) -> Option<Cause> {
    let mut bytes = [0u8; 16];
    let n = unsafe { libc::read(fd, bytes.as_mut_ptr().cast(), bytes.len()) };
    let mut cause = None;
    for byte in bytes.iter().take(n.max(0) as usize) {
        match i32::from(*byte) {
            libc::SIGWINCH => cause = cause.or(Some(Cause::Wake)),
            _ => return Some(Cause::Exit),
        }
    }
    cause
}

/// herdr's event socket, read without blocking.
struct Socket {
    stream: UnixStream,
    buffer: Vec<u8>,
    /// What the last `cause` that was a wake or an end came from, for the journal.
    why: String,
}
impl Socket {
    fn open(path: &Path, agent: Option<&str>) -> Option<Self> {
        let mut stream = UnixStream::connect(path).ok()?;
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .ok()?;
        stream
            .write_all(format!("{}\n", subscription(agent)).as_bytes())
            .ok()?;
        stream.set_nonblocking(true).ok()?;
        Some(Self {
            stream,
            buffer: vec![],
            why: String::new(),
        })
    }
    /// What the bytes that came mean. A socket that ended is a wake: the ordinary pane
    /// reconnects with its own patience.
    fn cause(
        &mut self,
        agent: Option<&str>,
        pane: Option<&str>,
        tab: Option<&str>,
    ) -> Option<Cause> {
        let mut chunk = [0u8; 4096];
        loop {
            match self.stream.read(&mut chunk) {
                Ok(0) => {
                    self.why = "herdr socket ended".into();
                    return Some(Cause::Wake);
                }
                Ok(n) => self.buffer.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    self.why = "herdr socket error".into();
                    return Some(Cause::Wake);
                }
            }
            if self.buffer.len() > 1 << 20 {
                self.why = "herdr stream too long".into();
                return Some(Cause::Wake);
            }
        }
        let mut found = None;
        while let Some(end) = self.buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=end).collect();
            if let Ok(value) = serde_json::from_slice::<Value>(&line) {
                match judge(&value, agent, pane, tab) {
                    Some(Cause::Exit) => {
                        self.why = describe(&value);
                        return Some(Cause::Exit);
                    }
                    Some(Cause::Wake) => {
                        found = Some(Cause::Wake);
                        self.why = describe(&value);
                    }
                    None => (),
                }
            }
        }
        found
    }
}

/// Take what the terminal holds: what came after the byte that woke the pane.
fn drain_stdin() -> Vec<u8> {
    let mut taken = vec![];
    for _ in 0..16 {
        let mut fd = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        if unsafe { libc::poll(&mut fd, 1, 0) } <= 0 || fd.revents & libc::POLLIN == 0 {
            break;
        }
        let mut bytes = [0u8; 256];
        let n = unsafe { libc::read(libc::STDIN_FILENO, bytes.as_mut_ptr().cast(), bytes.len()) };
        if n <= 0 {
            break;
        }
        taken.extend_from_slice(&bytes[..n as usize]);
    }
    taken
}
fn give_back_terminal() {
    let _ = terminal::disable_raw_mode();
    let _ = execute!(io::stdout(), DisableFocusChange, DisableMouseCapture);
    let _ = io::stdout().flush();
}

/// The transcript to watch: the one the pane that went to rest named, else the session's.
fn transcript_of(paths: &Paths, sid: &str) -> Option<PathBuf> {
    std::env::var_os(TRANSCRIPT)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| paths.transcript(sid))
}
/// The environment for the exec into resting: which transcript to watch, what it looked like
/// at the decision, and the descriptor handed over (if any).
pub fn rest_env(
    transcript: Option<&Path>,
    mark: Option<Mark>,
    fd: Option<RawFd>,
) -> Vec<(&'static str, OsString)> {
    let mut env = vec![
        (
            TRANSCRIPT,
            transcript.map_or_else(OsString::new, |p| p.as_os_str().to_owned()),
        ),
        (MARK, encode_mark(mark).into()),
    ];
    env.extend(fd.map(|fd| (WAKE_FD, fd.to_string().into())));
    env
}
/// The variables `rest_env` sets, for the exec back.
const REST_VARS: [&str; 3] = [TRANSCRIPT, MARK, WAKE_FD];
pub fn encode_mark(mark: Option<Mark>) -> String {
    mark.map_or_else(|| "none".into(), |offset| offset.to_string())
}
/// A mark as `encode_mark` wrote it: None for text that is not one (an older image's stamp
/// among them), Some(None) for no transcript.
pub fn decode_mark(text: &str) -> Option<Option<Mark>> {
    if text == "none" {
        return Some(None);
    }
    text.parse::<u64>().ok().map(Some)
}
/// Where to read the transcript from: where the pane that went to rest had read to, else the
/// transcript's end as it is now.
pub fn baseline(told: Option<&str>, transcript: Option<&Path>) -> Option<Mark> {
    told.and_then(decode_mark)
        .unwrap_or_else(|| transcript.and_then(mark))
}
/// The offset the pane that went to rest had read to, when it named the transcript it took it of. A pane
/// that had found no transcript yet names none, and a stamp of "nothing" must not be held against
/// a file the resting pane finds by the session's id: that would wake it at once, for ever.
pub fn told_mark(named: Option<OsString>, mark: Option<String>) -> Option<String> {
    named.filter(|p| !p.is_empty()).and(mark)
}
/// Whether a wake byte waits in the pipe already. Reads (and so empties) it.
pub fn pending_wake(pipe: Option<&mut wake::Listener>) -> bool {
    pipe.is_some_and(|pipe| {
        let (now, soon) = pipe.take();
        now || soon
    })
}

/// The resting process: wait for a wake, then become the ordinary pane again.
pub fn run(paths: Paths, args: &PaneArgs) -> anyhow::Result<()> {
    let running = std::env::current_exe()?;
    let home = std::env::var_os(watch::HOME_ENGINE)
        .map(PathBuf::from)
        .unwrap_or_else(|| running.clone());
    let sid = args.sid.as_deref().unwrap_or("");
    let agent = args.agent.as_deref();
    let (own_pane, own_tab) = (
        std::env::var("HERDR_PANE_ID").ok(),
        std::env::var("HERDR_TAB_ID").ok(),
    );
    let mut journal = Journal::new(&paths.keep, sid);
    let mut why = String::from("woken");
    let signals = signal_pipe();
    let pid_file = PidFile::new(&paths, sid)?;
    let mut pipe = wake::Listener::open(&paths.keep, sid);
    // The copy of the pipe's read end the pane handed over kept what waited in it, and kept the
    // relay's writes landing; this pipe has all of it now.
    if let (Some(pipe), Some(fd)) = (
        pipe.as_ref(),
        std::env::var(WAKE_FD)
            .ok()
            .and_then(|fd| fd.parse::<RawFd>().ok()),
    ) {
        pipe.release_inherited(fd);
    }
    // A pipe nobody writes to reports hang-up at once on some systems: hold a write end.
    let _writer = pipe.as_ref().and_then(wake::Listener::hold_open);
    let herdr = herdr_events::config(|name| std::env::var(name).ok());
    let mut socket = herdr.as_ref().and_then(|path| Socket::open(path, agent));
    let mut retry = Backoff::new(Instant::now());
    let raw = terminal::enable_raw_mode().is_ok();
    if raw {
        let _ = execute!(io::stdout(), EnableMouseCapture, EnableFocusChange);
    }
    let transcript = transcript_of(&paths, sid);
    let told = told_mark(std::env::var_os(TRANSCRIPT), std::env::var(MARK).ok());
    let mut tail = Tail::at(
        transcript.clone(),
        baseline(told.as_deref(), transcript.as_deref()),
    );
    let mut cause = Cause::Wake;
    // Whatever came between the pane's last look and this one wakes it at once: a wake byte, or
    // a turn in the transcript (lines that are not a turn are read past).
    let mut woken = false;
    if pending_wake(pipe.as_mut()) {
        woken = true;
        why = "wake byte waiting in the pipe".into();
    } else {
        let seen = tail.look(transcript.as_deref());
        if let Some(said) = seen.cause() {
            if seen.is_real() {
                woken = true;
                why = said;
            } else if matches!(seen, Activity::Ignored(_)) {
                journal.note(
                    Instant::now(),
                    Decision::Ignore,
                    &said,
                    None,
                    Focus::Unknown,
                );
            }
        }
    }
    let mut beat = Beat::new(Instant::now());
    let mut woke_with = vec![];
    loop {
        if woken {
            break;
        }
        let mut fds = vec![libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        }];
        let mut slot = |fd: Option<RawFd>| {
            fd.map(|fd| {
                fds.push(libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                });
                fds.len() - 1
            })
        };
        let at_signals = slot(signals);
        let at_pipe = slot(pipe.as_ref().map(wake::Listener::fd));
        let at_socket = slot(socket.as_ref().map(|s| s.stream.as_raw_fd()));
        let ready = unsafe {
            libc::poll(
                fds.as_mut_ptr(),
                fds.len() as libc::nfds_t,
                beat.wait_ms(Instant::now()),
            )
        };
        if ready < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }
        // On the deadline, whatever the wait returned for: events from herdr that come all the
        // time must not starve the pid file or the transcript.
        let now = Instant::now();
        if beat.due(now) {
            if let Some(path) = &pid_file.path {
                watch::touch(path);
            }
            let seen = tail.look(transcript.as_deref());
            if let Some(said) = seen.cause() {
                if seen.is_real() {
                    why = said;
                    break;
                }
                if matches!(seen, Activity::Ignored(_)) {
                    journal.note(now, Decision::Ignore, &said, None, Focus::Unknown);
                }
            }
            // A socket that would not open is tried again, now and then.
            if socket.is_none() && herdr.is_some() && retry.due(now) {
                socket = herdr.as_ref().and_then(|path| Socket::open(path, agent));
                if socket.is_none() {
                    retry.failed(now);
                }
            }
        }
        if ready == 0 {
            continue;
        }
        if let Some(found) = at_signals.and_then(|i| {
            (fds[i].revents & libc::POLLIN != 0)
                .then(|| signal_cause(fds[i].fd))
                .flatten()
        }) {
            cause = found;
            why = if found == Cause::Wake {
                "SIGWINCH (resize)".into()
            } else {
                "signal: an end was asked for".into()
            };
            break;
        }
        let mut typed_bytes = vec![];
        if let Some(found) = stdin_cause(fds[0].revents, || {
            typed_bytes = read_stdin();
            typed_bytes.clone()
        }) {
            cause = found;
            why = stdin_why(fds[0].revents, &typed_bytes).into();
            woke_with = typed_bytes;
            break;
        }
        if at_pipe.is_some_and(|i| fds[i].revents & libc::POLLIN != 0) {
            why = "wake byte on the pipe".into();
            break;
        }
        if at_socket.is_some_and(|i| fds[i].revents != 0) {
            if let Some(found) = socket
                .as_mut()
                .and_then(|s| s.cause(agent, own_pane.as_deref(), own_tab.as_deref()))
            {
                cause = found;
                why = socket.as_ref().map_or_else(String::new, |s| s.why.clone());
                break;
            }
        }
        if pipe.is_some() && at_pipe.is_some_and(|i| fds[i].revents & libc::POLLHUP != 0) {
            // Cannot happen with a writer held; if it does, do not spin.
            pipe = None;
        }
    }
    let focus = if why == "terminal focus-in" {
        Focus::In
    } else {
        Focus::Unknown
    };
    let decision = if cause == Cause::Exit {
        Decision::Exit
    } else {
        Decision::Wake
    };
    journal.write(decision, &why, None, focus, 1);
    if cause == Cause::Exit {
        if raw {
            give_back_terminal();
        }
        return Ok(());
    }
    // What woke the pane and what followed go to the new image: it replays scrolls, never keys.
    woke_with.extend(drain_stdin());
    if raw {
        give_back_terminal();
    }
    // The card stays on screen until the new image's first frame replaces it, all at once.
    let argv: Vec<OsString> = std::env::args_os().skip(1).collect();
    let mut back = std::process::Command::new(&running);
    back.args(without_dormant(&argv))
        .env(watch::HOME_ENGINE, &home)
        .env(
            super::wake_input::VARIABLE,
            super::wake_input::encode(&woke_with),
        )
        .envs(crate::malloc_role::carried());
    for var in REST_VARS {
        back.env_remove(var);
    }
    let error = back.exec();
    Err(anyhow::anyhow!("wake: {error}"))
}

/// The project's name for the card.
pub fn project_of(repo: &Path) -> String {
    repo.canonicalize()
        .unwrap_or_else(|_| repo.to_path_buf())
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}
/// Seconds since `path` last changed.
pub fn quiet_for(path: &Path) -> Option<u64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    SystemTime::now()
        .duration_since(modified)
        .ok()
        .map(|d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream as Pair;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_owned())
        }
    }
    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }
    const TEN: Duration = Duration::from_secs(600);

    #[test]
    fn five_minutes_unless_the_environment_says_otherwise_in_minutes_or_parts_of_one() {
        let five = Some(Duration::from_secs(300));
        assert_eq!(idle_limit(env(&[])), five);
        for (text, secs) in [("3", 180), (" 1 ", 60), ("0.5", 30), ("2.25", 135)] {
            let limit = idle_limit(env(&[("CSR_PANE_IDLE_MINS", text)]));
            assert_eq!(limit, Some(Duration::from_secs(secs)), "{text}");
        }
        for text in ["soon", "", "-1", "NaN", "inf"] {
            assert_eq!(
                idle_limit(env(&[("CSR_PANE_IDLE_MINS", text)])),
                five,
                "{text}"
            );
        }
    }
    #[test]
    fn zero_minutes_and_the_kill_switch_turn_resting_off() {
        assert_eq!(idle_limit(env(&[("CSR_PANE_IDLE_MINS", "0")])), None);
        assert_eq!(idle_limit(env(&[("CSR_PANE_NO_SLEEP", "1")])), None);
        assert_eq!(
            idle_limit(env(&[
                ("CSR_PANE_NO_SLEEP", "1"),
                ("CSR_PANE_IDLE_MINS", "5")
            ])),
            None
        );
        let off = Idle::new(None, Instant::now());
        assert!(!off.due(Instant::now() + Duration::from_secs(86_400)));
    }
    #[test]
    fn a_quiet_pane_is_due_at_the_limit_and_not_before() {
        let t0 = Instant::now();
        let idle = Idle::new(Some(TEN), t0);
        assert!(!idle.due(t0 + TEN - Duration::from_secs(1)));
        assert!(idle.due(t0 + TEN));
    }
    #[test]
    fn each_wake_source_starts_the_wait_over() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        for (name, seen) in [
            (
                "relay wake",
                Seen {
                    wake: true,
                    ..Seen::default()
                },
            ),
            (
                "key, click or resize",
                Seen {
                    input: true,
                    ..Seen::default()
                },
            ),
            (
                "agent working",
                Seen {
                    working: true,
                    ..Seen::default()
                },
            ),
        ] {
            let mut idle = Idle::new(Some(TEN), t0);
            idle.observe(at(500), seen);
            assert!(!idle.due(at(700)), "{name}");
            assert!(idle.due(at(1100)), "{name}");
        }
        // Nothing seen resets nothing.
        let mut idle = Idle::new(Some(TEN), t0);
        idle.observe(at(500), Seen::default());
        assert!(idle.due(at(600)));
    }
    /// A transcript file in a scratch folder, with `lines` in it.
    fn transcript_file(lines: &[&str]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("s.jsonl");
        let mut text = String::new();
        for line in lines {
            text.push_str(line);
            text.push('\n');
        }
        std::fs::write(&file, text).unwrap();
        (dir, file)
    }
    /// One look with no file identity to compare (the file's own length is all there is).
    fn act(file: &Path, offset: &mut u64) -> Activity {
        transcript_activity(file, offset, &mut None)
    }
    fn append(file: &Path, text: &str) {
        let mut f = std::fs::OpenOptions::new().append(true).open(file).unwrap();
        f.write_all(text.as_bytes()).unwrap();
    }
    const BRIDGE: &str = r#"{"type":"bridge-session","sessionId":"s","bridgeSessionId":"cse_1"}"#;
    const ASSISTANT: &str = r#"{"type":"assistant","message":{"content":[]}}"#;
    const PROMPT: &str = r#"{"type":"user","message":{"content":"hi"}}"#;
    #[test]
    fn transcript_activity_a_bridge_reconnect_is_not_a_turn() {
        let (_dir, file) = transcript_file(&[PROMPT, ASSISTANT]);
        let mut offset = mark(&file).unwrap();
        append(&file, &format!("{BRIDGE}\n"));
        append(
            &file,
            "{\"type\":\"session-owner\"}\n{\"type\":\"atis-latch\"}\n",
        );
        let before = offset;
        assert_eq!(
            act(&file, &mut offset),
            Activity::Ignored("bridge-session".into())
        );
        assert!(offset > before, "read past, so it is not looked at twice");
        assert_eq!(act(&file, &mut offset), Activity::Quiet);
        // The other lines Claude Code writes on its own account.
        for kind in [
            "queue-operation",
            "file-history-snapshot",
            "last-prompt",
            "ai-title",
            "mode",
            "permission-mode",
            "attachment",
            "system",
            "summary",
            "custom-title",
        ] {
            append(&file, &format!("{{\"type\":\"{kind}\"}}\n"));
            assert_eq!(
                act(&file, &mut offset),
                Activity::Ignored(kind.into()),
                "{kind}"
            );
        }
        // Not JSON at all: nothing to go on.
        append(&file, "half a log line\n");
        assert_eq!(
            act(&file, &mut offset),
            Activity::Ignored("unparseable".into())
        );
    }
    #[test]
    fn transcript_activity_an_assistant_line_or_a_prompt_is_a_turn() {
        let (_dir, file) = transcript_file(&[PROMPT]);
        let mut offset = mark(&file).unwrap();
        append(&file, &format!("{ASSISTANT}\n"));
        assert_eq!(act(&file, &mut offset), Activity::Real("assistant"));
        append(&file, &format!("{PROMPT}\n"));
        assert_eq!(act(&file, &mut offset), Activity::Real("user"));
        // A turn behind a bridge line still counts.
        append(&file, &format!("{BRIDGE}\n{ASSISTANT}\n"));
        assert_eq!(act(&file, &mut offset), Activity::Real("assistant"));
        // A tool result is a user line too.
        append(
            &file,
            "{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"tool_result\"}]}}\n",
        );
        assert!(act(&file, &mut offset).is_real());
        // But not what the harness wrote as meta, or a subagent's.
        append(&file, "{\"type\":\"user\",\"isMeta\":true}\n");
        assert_eq!(act(&file, &mut offset), Activity::Ignored("user".into()));
        append(&file, "{\"type\":\"user\",\"isSidechain\":true}\n");
        assert_eq!(act(&file, &mut offset), Activity::Ignored("user".into()));
    }
    #[test]
    fn transcript_activity_a_touch_with_no_growth_is_nothing() {
        let (_dir, file) = transcript_file(&[PROMPT]);
        let mut offset = mark(&file).unwrap();
        let later = SystemTime::now() + Duration::from_secs(120);
        std::fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert_eq!(act(&file, &mut offset), Activity::Quiet);
        // A file that is not there is quiet too.
        assert_eq!(
            act(&file.with_file_name("gone.jsonl"), &mut offset),
            Activity::Quiet
        );
    }
    #[test]
    fn transcript_activity_a_shorter_file_is_a_wake_not_a_silent_rebaseline() {
        let (_dir, file) = transcript_file(&[PROMPT, ASSISTANT, ASSISTANT]);
        let mut offset = mark(&file).unwrap();
        // The new, shorter file holds a turn nobody has judged.
        std::fs::write(&file, format!("{PROMPT}\n")).unwrap();
        assert_eq!(act(&file, &mut offset), Activity::Replaced);
        assert!(Activity::Replaced.is_real());
        assert_eq!(offset, mark(&file).unwrap());
        assert_eq!(act(&file, &mut offset), Activity::Quiet);
        append(&file, &format!("{ASSISTANT}\n"));
        assert!(act(&file, &mut offset).is_real());
    }
    #[test]
    fn transcript_activity_another_file_of_the_same_length_is_a_wake() {
        let (dir, file) = transcript_file(&[PROMPT]);
        let mut tail = Tail::default();
        assert_eq!(tail.look(Some(&file)), Activity::Rebaselined);
        assert_eq!(tail.look(Some(&file)), Activity::Quiet);
        // A new file of the very same length (a turn where the bridge line was) renamed over it.
        let same = PROMPT.replace("\"hi\"", "\"yo\"");
        assert_eq!(same.len(), PROMPT.len());
        let other = dir.path().join("new.jsonl");
        std::fs::write(&other, format!("{same}\n")).unwrap();
        std::fs::rename(&other, &file).unwrap();
        assert_eq!(tail.look(Some(&file)), Activity::Replaced);
        assert_eq!(tail.look(Some(&file)), Activity::Quiet, "once");
        // The very same file appended to is not replaced.
        append(&file, &format!("{BRIDGE}\n"));
        assert_eq!(
            tail.look(Some(&file)),
            Activity::Ignored("bridge-session".into())
        );
    }
    #[test]
    fn transcript_activity_the_watch_idle_clock_counts_a_replacement() {
        let t0 = Instant::now();
        let mut idle = Idle::new(Some(TEN), t0);
        idle.transcript(t0 + Duration::from_secs(500), &Activity::Replaced);
        assert!(!idle.due(t0 + Duration::from_secs(601)));
    }
    #[test]
    fn transcript_activity_half_a_line_waits_for_the_rest() {
        let (_dir, file) = transcript_file(&[PROMPT]);
        let mut offset = mark(&file).unwrap();
        let (head, tail) = ASSISTANT.split_at(20);
        append(&file, head);
        assert_eq!(act(&file, &mut offset), Activity::Quiet);
        append(&file, &format!("{tail}\n"));
        assert_eq!(act(&file, &mut offset), Activity::Real("assistant"));
        // A bridge line cut in two is judged whole, and ignored.
        let (head, tail) = BRIDGE.split_at(10);
        append(&file, head);
        assert_eq!(act(&file, &mut offset), Activity::Quiet);
        append(&file, &format!("{tail}\n"));
        assert_eq!(
            act(&file, &mut offset),
            Activity::Ignored("bridge-session".into())
        );
    }
    #[test]
    fn transcript_activity_a_big_append_of_noise_is_read_in_batches_and_is_no_turn() {
        let (_dir, file) = transcript_file(&[PROMPT]);
        let mut offset = mark(&file).unwrap();
        // About 3 MiB of bridge lines: far more than a batch, none of it a turn.
        append(&file, &format!("{BRIDGE}\n").repeat(45_000));
        let mut looks = 0;
        let mut last = Activity::Quiet;
        while offset < mark(&file).unwrap() && looks < 10 {
            last = act(&file, &mut offset);
            assert!(!last.is_real(), "{last:?}");
            looks += 1;
        }
        assert_eq!(last, Activity::Ignored("bridge-session".into()));
        assert_eq!(offset, mark(&file).unwrap(), "read to the last whole line");
        // A turn behind that much noise is found.
        append(&file, &format!("{BRIDGE}\n").repeat(30_000));
        append(&file, &format!("{ASSISTANT}\n"));
        let mut found = Activity::Quiet;
        for _ in 0..10 {
            found = act(&file, &mut offset);
            if found.is_real() {
                break;
            }
        }
        assert_eq!(found, Activity::Real("assistant"));
    }
    #[test]
    fn transcript_activity_noise_with_an_unfinished_record_keeps_the_offset_at_the_last_whole_line()
    {
        let (_dir, file) = transcript_file(&[PROMPT]);
        let mut offset = mark(&file).unwrap();
        append(&file, &format!("{BRIDGE}\n").repeat(20_000));
        append(&file, "{\"type\":\"bridge-sess");
        let whole = mark(&file).unwrap() - "{\"type\":\"bridge-sess".len() as u64;
        for _ in 0..5 {
            assert!(!act(&file, &mut offset).is_real());
        }
        assert_eq!(offset, whole);
    }
    #[test]
    fn transcript_activity_only_one_line_longer_than_a_batch_is_a_turn_unread() {
        let (_dir, file) = transcript_file(&[PROMPT]);
        let mut offset = mark(&file).unwrap();
        let long = format!(
            "{{\"type\":\"attachment\",\"pad\":\"{}\"}}\n",
            "x".repeat(BATCH + 10)
        );
        append(&file, &long);
        assert_eq!(act(&file, &mut offset), Activity::Real("oversized line"));
        assert!(offset > 0 && offset < mark(&file).unwrap());
    }
    #[test]
    fn the_tail_starts_from_the_end_follows_the_file_and_starts_over_on_another() {
        let (_dir, file) = transcript_file(&[PROMPT, ASSISTANT]);
        let mut tail = Tail::default();
        // The first look: what is there is history.
        assert_eq!(tail.look(Some(&file)), Activity::Rebaselined);
        assert_eq!(tail.offset(), mark(&file));
        assert_eq!(tail.look(Some(&file)), Activity::Quiet);
        append(&file, &format!("{BRIDGE}\n"));
        assert_eq!(
            tail.look(Some(&file)),
            Activity::Ignored("bridge-session".into())
        );
        append(&file, &format!("{ASSISTANT}\n"));
        assert!(tail.look(Some(&file)).is_real());
        assert_eq!(tail.look(None), Activity::Quiet);
        // Another file: its end is the new start.
        let (_other_dir, other) = transcript_file(&[ASSISTANT, ASSISTANT]);
        assert_eq!(tail.look(Some(&other)), Activity::Rebaselined);
        assert_eq!(tail.look(Some(&other)), Activity::Quiet);
        // Handed an offset: what came after it is new.
        let mut told = Tail::at(Some(file.clone()), Some(0));
        assert!(told.look(Some(&file)).is_real());
    }
    #[test]
    fn only_a_turn_in_the_own_transcript_starts_the_wait_over() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut idle = Idle::new(Some(TEN), t0);
        for quiet in [
            Activity::Quiet,
            Activity::Rebaselined,
            Activity::Ignored("bridge-session".into()),
        ] {
            idle.transcript(at(500), &quiet);
        }
        assert!(idle.due(at(601)), "a bridge reconnect is no activity");
        idle.transcript(at(400), &Activity::Real("assistant"));
        assert!(!idle.due(at(601)));
        assert!(idle.due(at(1000)));
    }
    #[test]
    fn a_deferred_pane_asks_again_after_the_wait() {
        let t0 = Instant::now();
        let mut idle = Idle::new(Some(TEN), t0);
        let now = t0 + TEN;
        assert!(idle.due(now));
        idle.defer(now, ASK_AGAIN);
        assert!(!idle.due(now + ASK_AGAIN - Duration::from_secs(1)));
        assert!(idle.due(now + ASK_AGAIN));
    }
    #[test]
    fn a_focused_pane_or_a_working_agent_never_rests() {
        let up =
            |focused: bool, status: &str| json!({ "focused": focused, "agent_status": status });
        let u = Focus::Unknown;
        assert_eq!(
            decide(Some(&up(false, "idle")), Some(&up(false, "unknown")), u),
            Verdict::Sleep
        );
        assert_eq!(
            decide(Some(&up(true, "idle")), Some(&up(false, "idle")), u),
            Verdict::Busy
        );
        assert_eq!(
            decide(Some(&up(false, "idle")), Some(&up(true, "idle")), u),
            Verdict::Busy
        );
        assert_eq!(
            decide(Some(&up(false, "working")), None, Focus::Out),
            Verdict::Busy
        );
        // herdr's word stands when it has one: the terminal's reports do not override it.
        assert_eq!(
            decide(Some(&up(false, "idle")), None, Focus::In),
            Verdict::Sleep
        );
    }
    #[test]
    fn without_a_word_from_herdr_only_a_terminal_that_looked_away_may_rest() {
        // Outside herdr, or herdr out of reach: unknown and focused never rest.
        assert_eq!(decide(None, None, Focus::Unknown), Verdict::Busy);
        assert_eq!(decide(None, None, Focus::In), Verdict::Busy);
        assert_eq!(decide(None, None, Focus::Out), Verdict::Sleep);
    }
    #[test]
    fn the_terminals_focus_reports_say_where_it_stands_and_looking_away_is_no_activity() {
        assert_eq!(Focus::of(&Event::FocusGained), Some(Focus::In));
        assert_eq!(Focus::of(&Event::FocusLost), Some(Focus::Out));
        assert_eq!(Focus::of(&Event::Resize(1, 1)), None);
        assert!(!is_activity(&Event::FocusLost));
        assert!(is_activity(&Event::FocusGained));
        assert!(is_activity(&Event::Resize(1, 1)));
        assert_eq!(Focus::default(), Focus::Unknown);
    }
    #[test]
    fn one_look_that_found_no_session_does_not_end_the_pane_ten_minutes_of_them_do() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut gone = Absence::default();
        gone.observe(at(0), &send::Session::Absent);
        assert!(gone.pending());
        assert!(!gone.confirmed(at(0)));
        assert!(!gone.confirmed(at(599)));
        // Looks every 5 s for ten minutes.
        for s in (5..=600).step_by(5) {
            gone.observe(at(s), &send::Session::Absent);
        }
        assert!(gone.confirmed(at(600)));
        // The word is not good for ever.
        assert!(!gone.confirmed(at(700)));
    }
    #[test]
    fn a_failed_look_ends_nothing_and_resets_nothing_and_a_session_starts_over() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut gone = Absence::default();
        for s in (0..=300).step_by(5) {
            gone.observe(at(s), &send::Session::Absent);
        }
        // Three minutes in which herdr could not be asked.
        for s in (305..=480).step_by(5) {
            gone.observe(at(s), &send::Session::Unknown);
        }
        assert!(!gone.confirmed(at(480)), "ten minutes were not looked at");
        for s in (485..=600).step_by(5) {
            gone.observe(at(s), &send::Session::Absent);
        }
        assert!(gone.confirmed(at(600)), "the failed looks cost nothing");
        // Only failures: nothing is known, nothing ends.
        let mut unknown = Absence::default();
        for s in (0..=1200).step_by(5) {
            unknown.observe(at(s), &send::Session::Unknown);
        }
        assert!(!unknown.pending() && !unknown.confirmed(at(1200)));
        // A session seen starts the ten minutes again.
        gone.observe(at(601), &send::Session::Id("a1325785".into()));
        assert!(!gone.pending() && !gone.confirmed(at(601)));
        gone.observe(at(602), &send::Session::Absent);
        assert!(!gone.confirmed(at(1100)));
    }
    #[test]
    fn the_beat_comes_on_its_deadline_even_when_the_wait_never_times_out() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut beat = Beat::new(t0);
        // herdr events every 200 ms: no wait ever ends by its timeout.
        let mut beats = vec![];
        for ms in (200..=16_000).step_by(200) {
            if beat.due(at(ms)) {
                beats.push(ms);
            }
        }
        assert_eq!(beats, [5000, 10_000, 15_000]);
        // The wait is cut to the deadline.
        let beat = Beat::new(t0);
        assert!((4000..=5001).contains(&beat.wait_ms(at(1000))));
        assert!(beat.wait_ms(at(9000)) >= 0);
    }
    #[test]
    fn a_socket_that_would_not_open_is_tried_again_less_and_less_often() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut retry = Backoff::new(t0);
        assert!(!retry.due(at(4)));
        assert!(retry.due(at(5)));
        retry.failed(at(5));
        assert!(!retry.due(at(14)));
        assert!(retry.due(at(15)));
        retry.failed(at(15));
        assert!(retry.due(at(35)));
        retry.failed(at(35));
        assert!(!retry.due(at(64)));
        assert!(retry.due(at(65)));
        retry.failed(at(65));
        assert!(retry.due(at(95)), "never slower than every 30 s");
    }
    #[test]
    fn what_the_pane_read_at_the_decision_is_where_the_resting_one_reads_from() {
        for m in [Some(42), Some(0), None] {
            assert_eq!(decode_mark(&encode_mark(m)), Some(m));
        }
        // An older image's stamp (length:secs:nanos) is no offset: read from the end.
        for bad in ["", "x", "1:2", "1:2:3", "-1", "1.5"] {
            assert_eq!(decode_mark(bad), None, "{bad}");
        }
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("s.jsonl");
        std::fs::write(&file, "{}\n").unwrap();
        let now = mark(&file);
        assert_eq!(now, Some(3));
        // Told: the told offset, even when the file has moved on since.
        let told = encode_mark(Some(1));
        assert_eq!(baseline(Some(&told), Some(&file)), Some(1));
        // Told there was none: none, even when a file is there now.
        assert_eq!(baseline(Some("none"), Some(&file)), None);
        // Not told, or told garbage: the file as it is.
        assert_eq!(baseline(None, Some(&file)), now);
        assert_eq!(baseline(Some("garbage"), Some(&file)), now);
        assert_eq!(baseline(Some("5:6:7"), Some(&file)), now);
        let env = rest_env(Some(&file), Some(1), Some(7));
        assert_eq!(env.len(), 3);
        assert!(env.iter().any(|(k, v)| *k == WAKE_FD && v == "7"));
        assert_eq!(rest_env(None, None, None).len(), 2);
    }
    #[test]
    fn a_stamp_of_nothing_is_not_held_against_a_transcript_found_later() {
        let stamp = Some(encode_mark(Some(1)));
        assert_eq!(told_mark(Some("/t/s.jsonl".into()), stamp.clone()), stamp);
        // The pane had no transcript to name: no stamp is told, the file as it is stands.
        assert_eq!(told_mark(Some("".into()), Some("none".into())), None);
        assert_eq!(told_mark(None, Some("none".into())), None);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("s.jsonl");
        std::fs::write(&file, "{}\n").unwrap();
        let told = told_mark(Some("".into()), Some("none".into()));
        assert_eq!(baseline(told.as_deref(), Some(&file)), mark(&file));
    }
    #[test]
    fn a_wake_byte_in_the_pipe_before_the_resting_pane_opens_it_wakes_it_at_once() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("panes")).unwrap();
        assert!(!pending_wake(None));
        // The pane that went to rest handed over a copy of the pipe's read end and ended.
        let first = wake::Listener::open_as(d.path(), "s", 4244).unwrap();
        let kept = first.hold_across_exec().unwrap();
        assert!(wake::notify(d.path(), "s", wake::Kind::Now));
        first.exec_away_for_test();
        // The resting image opens its pipe: the byte is there and it does not wait for a beat.
        let mut next = wake::Listener::open_as(d.path(), "s", 4244).unwrap();
        assert!(next.release_inherited(kept));
        assert!(pending_wake(Some(&mut next)));
        assert!(!pending_wake(Some(&mut next)), "read once");
    }
    #[test]
    fn a_focus_out_alone_does_not_wake_a_resting_pane_and_anything_else_does() {
        let out = b"\x1b[O";
        assert_eq!(typed(out), None);
        assert_eq!(typed(b"\x1b[O\x1b[O"), None);
        assert_eq!(typed(b"\x1b[I"), Some(Cause::Wake));
        assert_eq!(typed(b"\x1b[O\x1b[I"), Some(Cause::Wake));
        assert_eq!(typed(b"q"), Some(Cause::Wake));
        assert_eq!(typed(b"\x1b[O\x1b"), Some(Cause::Wake));
        assert_eq!(typed(b"\x1b[<0;5;5M"), Some(Cause::Wake));
        assert_eq!(typed(b""), Some(Cause::Wake));
    }
    #[test]
    fn every_verdict_says_why_for_the_journal() {
        let up =
            |focused: bool, status: &str| json!({ "focused": focused, "agent_status": status });
        let case = |agent: Option<Value>, own: Option<Value>, focus| {
            decide_why(agent.as_ref(), own.as_ref(), focus)
        };
        let u = Focus::Unknown;
        assert_eq!(
            case(Some(up(true, "idle")), Some(up(false, "idle")), u),
            (Verdict::Busy, "herdr says the agent pane is focused")
        );
        assert_eq!(
            case(Some(up(false, "idle")), Some(up(true, "idle")), u),
            (Verdict::Busy, "herdr says this pane is focused")
        );
        assert_eq!(
            case(Some(up(false, "working")), None, Focus::Out),
            (Verdict::Busy, "herdr says the agent is working")
        );
        assert_eq!(
            case(None, None, u),
            (
                Verdict::Busy,
                "no word from herdr and the terminal has not looked away"
            )
        );
        assert_eq!(
            case(None, None, Focus::Out),
            (Verdict::Sleep, "no word from herdr, terminal focus is out")
        );
        // herdr's word stands over a terminal that never reported: the case of every pane in herdr.
        assert_eq!(
            case(Some(up(false, "done")), Some(up(false, "unknown")), u),
            (Verdict::Sleep, "herdr says unfocused and not working")
        );
        assert_eq!(Focus::Unknown.word(), "unknown");
        assert_eq!(Focus::In.word(), "in");
        assert_eq!(Focus::Out.word(), "out");
    }
    #[test]
    fn what_woke_a_resting_pane_is_told_in_words() {
        assert_eq!(
            describe(&json!({ "event": "pane.focused", "data": {} })),
            "herdr event pane.focused"
        );
        assert_eq!(
            describe(&json!({ "error": { "code": "events_lost" } })),
            "herdr error events_lost"
        );
        assert_eq!(stdin_why(0, b""), "terminal read came back empty");
        assert_eq!(stdin_why(0, b"\x1b[I"), "terminal focus-in");
        assert_eq!(stdin_why(0, b"\x1b[O\x1b[I"), "terminal focus-in");
        assert_eq!(stdin_why(0, b"\x1b[<0;5;5M"), "click or scroll");
        assert_eq!(stdin_why(0, b"q"), "key");
        assert_eq!(stdin_why(libc::POLLHUP, b""), "terminal hung up");
        assert_eq!(
            stdin_why(libc::POLLNVAL, b""),
            "terminal could not be polled"
        );
    }
    #[test]
    fn the_arguments_come_back_to_watch_as_they_went() {
        let original = os(&[
            "companion",
            "pane",
            "--repo",
            "/r/p",
            "--watch",
            "--sid",
            "abc",
            "--agent",
            "w1:p1",
        ]);
        let resting = with_dormant(&original);
        assert_eq!(resting.last().unwrap(), FLAG);
        assert_eq!(resting.len(), original.len() + 1);
        assert_eq!(without_dormant(&resting), original);
        // Idempotent: a flag already there is not doubled.
        assert_eq!(with_dormant(&resting), resting);
    }
    #[test]
    fn the_sid_the_pane_runs_on_goes_with_it_into_rest() {
        let argv = os(&["pane", "--watch", "--sid", "old", "--agent", "w1:p1"]);
        assert_eq!(carrying_sid(&argv, "old"), argv);
        assert_eq!(
            carrying_sid(&os(&["pane", "--sid=old", "--watch"]), "old"),
            os(&["pane", "--sid=old", "--watch"])
        );
        let moved = carrying_sid(&argv, "new");
        assert_eq!(
            moved,
            os(&["pane", "--watch", "--agent", "w1:p1", "--sid", "new"])
        );
        assert_eq!(without_dormant(&with_dormant(&moved)), moved);
    }
    #[test]
    fn the_card_is_words_and_says_what_wakes_it() {
        assert_eq!(age(10), "under a minute");
        assert_eq!(age(35 * 60), "35 min");
        assert_eq!(age(2 * 3600), "2 h");
        assert_eq!(age(3 * 86_400), "3 d");
        let wide = card("this-ai-life", Some(7200), 120);
        assert!(
            wide.contains("CSR is resting. this-ai-life, last turn 2 h ago."),
            "{wide}"
        );
        assert!(
            wide.contains("Wakes on your next prompt, a key, or a click."),
            "{wide}"
        );
        assert!(wide.starts_with("\x1b[2J"));
        let narrow = card("this-ai-life", None, 35);
        assert!(!narrow.contains("last turn"));
        let rows: Vec<&str> = narrow.split("\x1b[").skip(3).collect();
        assert!(rows.len() >= 3, "{narrow:?}");
        for row in rows {
            let text = row.split_once('H').map_or("", |(_, t)| t);
            assert!(text.chars().count() <= 33, "{text:?}");
        }
    }
    #[test]
    fn herdr_events_wake_or_end_a_resting_pane_only_when_they_are_about_it() {
        let say = |event: &str, data: Value| json!({ "event": event, "data": data });
        let judged = |line: Value| judge(&line, Some("w1:p1"), Some("w1:p2"), Some("w1:t1"));
        assert_eq!(
            judged(say(
                "pane.agent_status_changed",
                json!({ "pane_id": "w1:p1", "agent_status": "working" })
            )),
            Some(Cause::Wake)
        );
        assert_eq!(
            judged(say(
                "pane.agent_status_changed",
                json!({ "pane_id": "w1:p1", "agent_status": "idle" })
            )),
            None
        );
        assert_eq!(
            judged(say(
                "pane.agent_status_changed",
                json!({ "pane_id": "w9:p1", "agent_status": "working" })
            )),
            None
        );
        assert_eq!(
            judged(say("pane_focused", json!({ "pane_id": "w1:p1" }))),
            Some(Cause::Wake)
        );
        assert_eq!(
            judged(say("pane.focused", json!({ "pane_id": "w1:p2" }))),
            Some(Cause::Wake)
        );
        assert_eq!(
            judged(say("pane.focused", json!({ "pane_id": "w3:p1" }))),
            None
        );
        assert_eq!(
            judged(say("tab.focused", json!({ "tab_id": "w1:t1" }))),
            Some(Cause::Wake)
        );
        assert_eq!(
            judged(say("tab.focused", json!({ "tab_id": "w1:t2" }))),
            None
        );
        assert_eq!(
            judged(say("pane.exited", json!({ "pane_id": "w1:p1" }))),
            Some(Cause::Exit)
        );
        assert_eq!(
            judged(say("pane.closed", json!({ "pane_id": "w1:p2" }))),
            None
        );
        assert_eq!(
            judged(json!({ "error": { "code": "events_lost" } })),
            Some(Cause::Wake)
        );
        assert_eq!(
            judged(json!({ "result": { "type": "subscription_started" } })),
            None
        );
        // Without the ids in the environment nothing matches an absent id.
        assert_eq!(
            judge(
                &say("pane.focused", json!({ "pane_id": "w1:p1" })),
                None,
                None,
                None
            ),
            None
        );
    }
    #[test]
    fn the_subscription_asks_for_focus_status_and_the_end() {
        let said: Value = serde_json::from_str(&subscription(Some("w1:p1"))).unwrap();
        let types: Vec<&str> = said["params"]["subscriptions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|s| s["type"].as_str())
            .collect();
        for t in [
            "pane.focused",
            "tab.focused",
            "pane.closed",
            "pane.exited",
            "pane.agent_status_changed",
        ] {
            assert!(types.contains(&t), "{t}");
        }
        let alone: Value = serde_json::from_str(&subscription(None)).unwrap();
        assert_eq!(
            alone["params"]["subscriptions"].as_array().unwrap().len(),
            4
        );
    }
    #[test]
    fn the_terminal_hanging_up_ends_it_and_a_byte_wakes_it() {
        let key = || b"k".to_vec();
        let unread = || -> Vec<u8> { panic!("nothing to read") };
        assert_eq!(stdin_cause(libc::POLLIN, key), Some(Cause::Wake));
        assert_eq!(
            stdin_cause(libc::POLLIN | libc::POLLHUP, key),
            Some(Cause::Exit)
        );
        assert_eq!(stdin_cause(libc::POLLERR, unread), Some(Cause::Exit));
        assert_eq!(stdin_cause(libc::POLLNVAL, unread), Some(Cause::Wake));
        assert_eq!(stdin_cause(0, unread), None);
        // A window that lost focus is not someone at the keys.
        assert_eq!(stdin_cause(libc::POLLIN, || b"\x1b[O".to_vec()), None);
        assert_eq!(
            stdin_cause(libc::POLLIN, || b"\x1b[I".to_vec()),
            Some(Cause::Wake)
        );
    }
    #[test]
    fn a_resize_wakes_and_an_end_ends_through_the_signal_pipe() {
        let mut fds = [0 as libc::c_int; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let put = |sig: libc::c_int| {
            let byte = sig as u8;
            unsafe { libc::write(fds[1], (&byte as *const u8).cast(), 1) };
        };
        put(libc::SIGWINCH);
        assert_eq!(signal_cause(fds[0]), Some(Cause::Wake));
        put(libc::SIGTERM);
        assert_eq!(signal_cause(fds[0]), Some(Cause::Exit));
        put(libc::SIGWINCH);
        put(libc::SIGHUP);
        assert_eq!(signal_cause(fds[0]), Some(Cause::Exit));
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }
    #[test]
    fn herdr_lines_split_across_reads_are_read_whole_and_a_closed_socket_wakes() {
        let (mine, theirs) = Pair::pair().unwrap();
        mine.set_nonblocking(true).unwrap();
        let mut socket = Socket {
            stream: mine,
            buffer: vec![],
            why: String::new(),
        };
        let mut herdr = theirs;
        let ask = |s: &mut Socket| s.cause(Some("w1:p1"), None, None);
        assert_eq!(ask(&mut socket), None, "nothing yet");
        herdr
            .write_all(b"{\"result\":{\"type\":\"subscription_started\"}}\n{\"event\":\"pane.fo")
            .unwrap();
        assert_eq!(ask(&mut socket), None);
        herdr
            .write_all(b"cused\",\"data\":{\"pane_id\":\"w1:p1\"}}\n")
            .unwrap();
        // The write may be seen a moment later.
        let mut got = None;
        for _ in 0..100 {
            got = ask(&mut socket);
            if got.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(got, Some(Cause::Wake));
        assert_eq!(socket.why, "herdr event pane.focused");
        drop(herdr);
        let mut ended = None;
        for _ in 0..100 {
            ended = ask(&mut socket);
            if ended.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(ended, Some(Cause::Wake));
    }
    #[test]
    fn a_resting_pane_keeps_its_pid_file_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("s.json");
        std::fs::write(&file, "{}").unwrap();
        let old = SystemTime::now() - Duration::from_secs(3600);
        std::fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(old)
            .unwrap();
        watch::touch(&file);
        let age = SystemTime::now()
            .duration_since(std::fs::metadata(&file).unwrap().modified().unwrap())
            .unwrap();
        assert!(age < Duration::from_secs(5), "{age:?}");
        // Often enough that nothing reading the file's age takes it for dead: the ordinary
        // heartbeat stops at 120 s.
        assert!(BEAT * 4 < Duration::from_secs(120));
    }
    #[test]
    fn the_card_names_the_project_and_the_last_turn_from_the_files() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("this-ai-life");
        std::fs::create_dir(&repo).unwrap();
        assert_eq!(project_of(&repo), "this-ai-life");
        let transcript = dir.path().join("s.jsonl");
        std::fs::write(&transcript, "{}\n").unwrap();
        assert!(quiet_for(&transcript).unwrap() < 5);
        assert_eq!(quiet_for(&dir.path().join("none")), None);
        assert!(mark(&transcript).is_some());
        assert_eq!(mark(&dir.path().join("none")), None);
    }
}
