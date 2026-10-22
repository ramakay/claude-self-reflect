//! Waking a pane: the relay tells a pane its session did something, the pane draws.
//!
//! The relay (`companion serve`) sees every hook of its Claude Code session. On the ones that
//! change what a pane shows it sends a signal to the pane's pid, which `watch` keeps in
//! `<keep>/panes/<sid>.json`:
//!
//! - `SIGUSR1`, at once: a turn ended, a prompt was sent, a compaction began.
//! - `SIGUSR2`, soon: a tool finished, a measurement came. These arrive in bursts, so the pane
//!   waits 300 ms for the burst to gather.
//!
//! Signals only lower the latency of a redraw. The pane's own checks (`gate.rs`) are correct
//! without them, for the sessions whose relay cannot reach them.
//!
//! A signal that nothing catches ends the process, so the relay signals only a pane that wrote
//! `"wake": 1` into its pid file after it installed its handlers, and only while that file is
//! being kept fresh by the pane's heartbeat.
use super::paths::{read_json, valid_id};
use std::{
    path::Path,
    time::{Duration, Instant, SystemTime},
};

/// What a hook means for a pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Draw now.
    Now,
    /// Draw soon: more of the same are likely to follow.
    Burst,
}
impl Kind {
    pub fn signal(self) -> libc::c_int {
        match self {
            Kind::Now => libc::SIGUSR1,
            Kind::Burst => libc::SIGUSR2,
        }
    }
}

/// What a relay request is to a pane: `path` is `/event` or `/after`, `key` the hook's key.
pub fn classify(path: &str, key: &str) -> Option<Kind> {
    match (path, key) {
        // Stop, prompt submit, PreCompact, and a subagent's end.
        (
            "/event",
            "stop" | "turn_start" | "turn_complete" | "seen" | "digit" | "chapter" | "marker",
        )
        | ("/after", "spawned") => Some(Kind::Now),
        // PostToolUse, and the usage figures that follow a reply.
        ("/after", "tool") | ("/event", "measure") => Some(Kind::Burst),
        _ => None,
    }
}

/// A pid file older than this is not being kept by a living pane (its heartbeat is every 5 s).
const FRESH: Duration = Duration::from_secs(20);

/// Signal the pane of `sid`, if there is one that can take it. True when a signal was sent.
pub fn notify(keep: &Path, sid: &str, kind: Kind) -> bool {
    if !valid_id(sid) {
        return false;
    }
    let file = keep.join("panes").join(format!("{sid}.json"));
    let fresh = std::fs::metadata(&file)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age <= FRESH);
    if !fresh {
        return false;
    }
    let pane = read_json(&file);
    let pid = pane["pid"]
        .as_i64()
        .filter(|p| (2..=i64::from(i32::MAX)).contains(p));
    match pid {
        Some(pid) if pane["wake"] == 1 => unsafe { libc::kill(pid as i32, kind.signal()) == 0 },
        _ => false,
    }
}

/// How long a burst gathers before its draw.
pub const GATHER: Duration = Duration::from_millis(300);
/// No more than one wake's draw in this long.
pub const SPACING: Duration = Duration::from_secs(1);

/// When the signals a pane got become draws: a leading edge for `Now`, a trailing one for
/// `Burst`, and never closer than `SPACING`. A pure clock-in, answer-out struct.
#[derive(Default)]
pub struct Debounce {
    want: Option<Instant>,
    drew: Option<Instant>,
}
impl Debounce {
    pub fn signal(&mut self, at: Instant, kind: Kind) {
        let due = match kind {
            Kind::Now => at,
            Kind::Burst => at + GATHER,
        };
        self.want = Some(self.want.map_or(due, |w| w.min(due)));
    }
    /// A draw happened at `at`, for whatever reason.
    pub fn drew(&mut self, at: Instant) {
        self.drew = Some(at);
    }
    /// Whether a wake's draw is due at `at`. True once per wake.
    pub fn ready(&mut self, at: Instant) -> bool {
        let Some(want) = self.want else {
            return false;
        };
        let open = self
            .drew
            .is_none_or(|d| at.saturating_duration_since(d) >= SPACING);
        if want > at || !open {
            return false;
        }
        self.want = None;
        true
    }
    /// When to look again: the wake's draw is due, or the spacing ends.
    pub fn next(&self) -> Option<Instant> {
        let want = self.want?;
        Some(self.drew.map_or(want, |d| want.max(d + SPACING)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn the_hooks_that_change_a_pane_are_told_apart_by_how_soon() {
        for key in ["stop", "seen", "digit", "turn_start", "chapter", "marker"] {
            assert_eq!(classify("/event", key), Some(Kind::Now), "{key}");
        }
        assert_eq!(classify("/after", "spawned"), Some(Kind::Now));
        assert_eq!(classify("/after", "tool"), Some(Kind::Burst));
        assert_eq!(classify("/event", "measure"), Some(Kind::Burst));
        // Before a tool runs, a render, a config: nothing new to show.
        for (path, key) in [
            ("/event", "tool"),
            ("/event", "band"),
            ("/after", "stop"),
            ("/step", "step"),
            ("/event", "config"),
        ] {
            assert_eq!(classify(path, key), None, "{path} {key}");
        }
    }

    #[test]
    fn fifty_signals_in_a_second_draw_at_most_twice() {
        let t0 = Instant::now();
        let mut wake = Debounce::default();
        let mut draws = 0;
        for tick in 0..=1200 {
            let at = t0 + Duration::from_millis(tick);
            if tick < 1000 && tick % 20 == 0 {
                wake.signal(at, Kind::Now);
            }
            if wake.ready(at) {
                wake.drew(at);
                draws += 1;
            }
        }
        assert_eq!(
            draws, 2,
            "the first at once, the rest together a second later"
        );
    }

    #[test]
    fn a_burst_of_tools_gathers_for_300_ms_and_a_turn_end_does_not_wait() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut wake = Debounce::default();
        wake.signal(ms(0), Kind::Burst);
        wake.signal(ms(100), Kind::Burst);
        assert!(!wake.ready(ms(299)));
        assert!(wake.ready(ms(300)));
        wake.drew(ms(300));
        assert!(!wake.ready(ms(301)), "nothing left to draw");
        // A Stop wins over a gathering burst.
        wake.signal(ms(2000), Kind::Burst);
        wake.signal(ms(2010), Kind::Now);
        assert!(wake.ready(ms(2010)));
    }

    #[test]
    fn a_wake_never_draws_closer_than_a_second_to_the_last_draw() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut wake = Debounce::default();
        wake.drew(ms(0));
        wake.signal(ms(100), Kind::Now);
        assert!(!wake.ready(ms(999)));
        assert_eq!(wake.next(), Some(ms(1000)));
        assert!(wake.ready(ms(1000)));
    }

    fn pane_file(keep: &Path, sid: &str, pid: u32, wake: bool) {
        std::fs::create_dir_all(keep.join("panes")).unwrap();
        let body = if wake {
            format!("{{\"pid\":{pid},\"wake\":1}}")
        } else {
            format!("{{\"pid\":{pid}}}")
        };
        std::fs::write(keep.join("panes").join(format!("{sid}.json")), body).unwrap();
    }

    #[test]
    fn only_a_fresh_pane_that_said_it_takes_signals_is_signalled() {
        let d = tempfile::tempdir().unwrap();
        // A child that catches nothing would die of SIGUSR1: it must not be sent one unless
        // it asked. `sleep` is that child.
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        pane_file(d.path(), "old-pane", pid, false);
        assert!(!notify(d.path(), "old-pane", Kind::Now), "no wake field");
        assert!(child.try_wait().unwrap().is_none(), "the pane is alive");
        assert!(!notify(d.path(), "missing", Kind::Now));
        assert!(!notify(d.path(), "../escape", Kind::Now));
        pane_file(d.path(), "stale", pid, true);
        let file = d.path().join("panes/stale.json");
        let old = SystemTime::now() - Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert!(
            !notify(d.path(), "stale", Kind::Now),
            "its heartbeat stopped"
        );
        assert!(child.try_wait().unwrap().is_none());
        // Pid 1 and nonsense are never signalled.
        pane_file(d.path(), "init", 1, true);
        assert!(!notify(d.path(), "init", Kind::Now));
        pane_file(d.path(), "live", pid, true);
        assert!(notify(d.path(), "live", Kind::Now));
        let status = child.wait().unwrap();
        assert!(!status.success(), "sleep takes the signal as its end");
    }
}
