//! When Claude Code's own auto-compact beats CSR's compactor, CSR says so once, with the count,
//! and says how to let CSR lead. Advice only: CSR never edits settings.json.
//!
//! With `autoCompactWindow: 300000` the native compaction fires near 267k (the window less
//! `NATIVE_UNDER`). CSR's compactor acts at a turn end, from `force_at` (247k), so a long turn
//! that crosses it without ending is compacted natively first.
//!
//! Evidence, all of it already on disk:
//!  - the ledger `native-wins.jsonl` in the companion's folder: the compactor's `marker` hook
//!    writes one line for every compaction Claude Code starts on its own (`trigger: "auto"`),
//!    with the compactor's setting at that moment. No transcript is read to count them.
//!  - the window in force, read as the compactor and the Context bar read it.
//!
//! The gate shows the warning only when a window is in force, the window less 33k sits at or
//! below the band's high edge (a window far past the band is not what beat CSR), and at least one
//! native compaction of the last seven days came while the compactor was not Off.
//!
//! One slot a day: the first session to ask holds it for 24 hours; the pane shows the notice for
//! `PANE_LIFE` and the SessionStart recap carries one clause, both inside that session.
//! `CSR_NO_NATIVE_WARN=1` turns it off.

use super::{
    app::{
        compactor::rules::{native_window_of, NATIVE_UNDER},
        state::BAND_HIGH,
    },
    paths::{read_json, Paths},
    private,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{fs, path::Path};

/// How far back a native win counts.
pub const WEEK: f64 = 7. * 86400.;
/// One warning per this long, across sessions.
pub const DAY: f64 = 86400.;
/// How long the pane's notice stays up once it is shown.
pub const PANE_LIFE: f64 = 300.;
const LEDGER: &str = "native-wins.jsonl";
const STATE: &str = "native-warn.json";
/// The ledger keeps this many lines at most.
const LEDGER_LINES: usize = 200;
/// A ledger or state file past this is not read.
const FILE_MAX: u64 = 512 * 1024;

/// Whether `CSR_NO_NATIVE_WARN` holds a "yes".
pub fn killed(value: Option<&str>) -> bool {
    value.is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

/// Whether the kill switch is set in this process.
pub fn disabled() -> bool {
    killed(std::env::var("CSR_NO_NATIVE_WARN").ok().as_deref())
}

/// One compaction Claude Code started on its own.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Win {
    pub at: f64,
    #[serde(default)]
    pub sid: String,
    /// The compactor's setting then: `off`, `suggest` or `auto`.
    #[serde(default)]
    pub compactor: String,
}

/// Where the window in force comes from, which decides what to tell the person to remove.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Settings,
    Env,
}

/// The source of a window `native_window_of` found: the environment wins when it parses.
pub fn source_of(env_window: Option<&str>) -> Source {
    if env_window
        .and_then(|t| t.trim().parse::<f64>().ok())
        .is_some()
    {
        Source::Env
    } else {
        Source::Settings
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Warning {
    pub window: f64,
    pub wins: usize,
    pub source: Source,
}

impl Warning {
    /// The one line, in STE-100.
    pub fn line(&self) -> String {
        let k = (self.window / 1000.).round() as u64;
        let fix = match self.source {
            Source::Settings => "Remove autoCompactWindow from settings.json",
            Source::Env => "Unset CLAUDE_CODE_AUTO_COMPACT_WINDOW",
        };
        format!(
            "Claude's {k}k auto-compact window beat CSR {}\u{d7} this week. {fix} to let CSR lead.",
            self.wins
        )
    }
}

/// The gate. None unless a window is in force, it sits at or below the band's high edge once the
/// native buffer is taken off, and a native compaction of the last week came while the compactor
/// was not Off.
pub fn warning(window: Option<f64>, source: Source, wins: &[Win], now: f64) -> Option<Warning> {
    let window = window.filter(|w| *w > 0.)?;
    if window - NATIVE_UNDER > BAND_HIGH {
        return None;
    }
    let n = wins
        .iter()
        .filter(|w| w.compactor != "off" && now - WEEK <= w.at && w.at <= now + 60.)
        .count();
    (n > 0).then_some(Warning {
        window,
        wins: n,
        source,
    })
}

/// The wins in the ledger; none when it is missing, large or unreadable.
pub fn load_wins(paths: &Paths) -> Vec<Win> {
    let path = paths.keep.join(LEDGER);
    if fs::metadata(&path).map_or(true, |m| m.len() > FILE_MAX) {
        return vec![];
    }
    fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Win>(line).ok())
        .collect()
}

/// Keep one native compaction in the ledger, dropping lines older than a week and capping the
/// file. Called from the `marker` hook, so it never fails: an error is nothing.
pub fn record_win(paths: &Paths, win: &Win) {
    let keep = |w: &Win| win.at - WEEK <= w.at;
    let mut wins: Vec<Win> = load_wins(paths).into_iter().filter(keep).collect();
    wins.push(win.clone());
    let from = wins.len().saturating_sub(LEDGER_LINES);
    let text: String = wins[from..]
        .iter()
        .filter_map(|w| serde_json::to_string(w).ok())
        .map(|line| line + "\n")
        .collect();
    let _ = private::write_atomic(&paths.keep.join(LEDGER), &text, &paths.keep);
}

/// The window, its source, and the ledger, read as the surfaces read them: `settings` is
/// settings.json.
pub fn evaluate(
    paths: &Paths,
    settings: &Value,
    env_window: Option<&str>,
    disabled_native: bool,
    now: f64,
) -> Option<Warning> {
    let window = native_window_of(settings, env_window, disabled_native)?;
    warning(Some(window), source_of(env_window), &load_wins(paths), now)
}

/// `evaluate` from this process's own settings.json and environment.
pub fn evaluate_here(paths: &Paths, now: f64) -> Option<Warning> {
    if disabled() {
        return None;
    }
    let settings = read_json(&paths.home.join(".claude/settings.json"));
    let env = std::env::var("CLAUDE_CODE_AUTO_COMPACT_WINDOW").ok();
    evaluate(
        paths,
        &settings,
        env.as_deref(),
        std::env::var_os("DISABLE_AUTO_COMPACT").is_some(),
        now,
    )
}

// ---------------------------------------------------------------- the daily slot

#[derive(Default, Serialize, Deserialize)]
struct State {
    claimed_at: f64,
    sid: String,
    /// When the pane first showed the notice in the claiming session.
    pane_at: Option<f64>,
    /// Whether the claiming session's recap carried the clause.
    recap: bool,
}

fn load_state(paths: &Paths) -> State {
    let path = paths.keep.join(STATE);
    if fs::metadata(&path).map_or(true, |m| m.len() > FILE_MAX) {
        return State::default();
    }
    serde_json::from_value(read_json(&path)).unwrap_or_default()
}

fn save_state(paths: &Paths, state: &State) {
    if let Ok(text) = serde_json::to_string(state) {
        let _ = private::write_atomic(&paths.keep.join(STATE), &text, &paths.keep);
    }
}

/// Whether another session holds today's slot.
fn held_by_other(state: &State, sid: &str, now: f64) -> bool {
    state.sid != sid
        && state.claimed_at > 0.
        && now - state.claimed_at < DAY
        && state.claimed_at <= now + 60.
}

/// What the pane may do for session `sid`.
#[derive(Debug, PartialEq)]
pub enum PaneSlot {
    /// Not this session's, or already shown and gone.
    No,
    /// This session may show it now: call `pane_shown`.
    Fresh,
    /// Shown, still up until this time.
    Live { until: f64 },
}

pub fn pane_slot(paths: &Paths, sid: &str, now: f64) -> PaneSlot {
    let state = load_state(paths);
    if held_by_other(&state, sid, now) {
        return PaneSlot::No;
    }
    match state.pane_at {
        Some(at) if state.sid == sid && now - state.claimed_at < DAY => {
            if now - at < PANE_LIFE {
                PaneSlot::Live {
                    until: at + PANE_LIFE,
                }
            } else {
                PaneSlot::No
            }
        }
        _ => PaneSlot::Fresh,
    }
}

/// The pane showed the notice in `sid` at `now`.
pub fn pane_shown(paths: &Paths, sid: &str, now: f64) {
    let mut state = load_state(paths);
    if state.sid != sid || now - state.claimed_at >= DAY {
        state = State {
            claimed_at: now,
            sid: sid.to_string(),
            ..State::default()
        };
    }
    state.pane_at = Some(now);
    save_state(paths, &state);
}

/// Whether the recap of session `sid` may carry the clause.
pub fn recap_slot(paths: &Paths, sid: &str, now: f64) -> bool {
    let state = load_state(paths);
    !held_by_other(&state, sid, now)
        && !(state.sid == sid && state.recap && now - state.claimed_at < DAY)
}

/// The recap of `sid` carried the clause.
pub fn recap_shown(paths: &Paths, sid: &str, now: f64) {
    let mut state = load_state(paths);
    if state.sid != sid || now - state.claimed_at >= DAY {
        state = State {
            claimed_at: now,
            sid: sid.to_string(),
            ..State::default()
        };
    }
    state.recap = true;
    save_state(paths, &state);
}

/// The clause for the SessionStart recap of `sid`: the line, when the gate passes and the slot is
/// this session's. Call `recap_shown` once the clause is in the paragraph.
pub fn recap_line(paths: &Paths, sid: &str, now: f64) -> Option<String> {
    let warning = evaluate_here(paths, now)?;
    recap_line_of(paths, sid, now, warning)
}

fn recap_line_of(paths: &Paths, sid: &str, now: f64, warning: Warning) -> Option<String> {
    if sid.is_empty() {
        return None;
    }
    recap_slot(paths, sid, now).then(|| warning.line())
}

/// Used by tests and tools that need the folder to exist.
pub fn ledger_path(keep: &Path) -> std::path::PathBuf {
    keep.join(LEDGER)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: f64 = 1_800_000_000.;

    fn win(ago: f64, compactor: &str) -> Win {
        Win {
            at: NOW - ago,
            sid: "s".into(),
            compactor: compactor.into(),
        }
    }
    fn gate(window: Option<f64>, wins: &[Win]) -> Option<Warning> {
        warning(window, Source::Settings, wins, NOW)
    }
    fn home() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().to_path_buf());
        fs::create_dir_all(&paths.keep).unwrap();
        (dir, paths)
    }

    #[test]
    fn a_window_unset_gives_no_warning() {
        let wins = [win(3600., "auto"), win(7200., "auto")];
        assert_eq!(gate(None, &wins), None);
        // as the compactor reads it: unset, off by the env, or off in settings
        assert_eq!(native_window_of(&json!({}), None, false), None);
        let (_d, paths) = home();
        record_win(&paths, &win(60., "auto"));
        assert_eq!(evaluate(&paths, &json!({}), None, false, NOW), None);
        assert_eq!(
            evaluate(
                &paths,
                &json!({"autoCompactWindow": 300000}),
                None,
                true,
                NOW
            ),
            None,
            "DISABLE_AUTO_COMPACT"
        );
        assert_eq!(
            evaluate(
                &paths,
                &json!({"autoCompactWindow": 300000, "autoCompactEnabled": false}),
                None,
                false,
                NOW
            ),
            None,
            "auto-compact off in settings"
        );
    }

    #[test]
    fn a_300k_window_with_no_native_auto_compaction_gives_no_warning() {
        assert_eq!(gate(Some(300_000.), &[]), None);
    }

    #[test]
    fn a_300k_window_and_two_native_wins_gives_the_line_with_n_2() {
        let wins = [win(3600., "auto"), win(2. * 86400., "suggest")];
        let w = gate(Some(300_000.), &wins).expect("two wins in the week");
        assert_eq!(w.wins, 2);
        assert_eq!(
            w.line(),
            "Claude's 300k auto-compact window beat CSR 2\u{d7} this week. \
             Remove autoCompactWindow from settings.json to let CSR lead."
        );
    }

    #[test]
    fn a_1m_window_gives_no_warning_because_native_fires_past_the_band() {
        let wins = [win(3600., "auto"), win(7200., "auto")];
        assert_eq!(gate(Some(1_000_000.), &wins), None);
        // the edge: window - 33k at the band's high edge counts, a token past it does not
        assert!(gate(Some(BAND_HIGH + NATIVE_UNDER), &wins).is_some());
        assert_eq!(gate(Some(BAND_HIGH + NATIVE_UNDER + 1.), &wins), None);
    }

    #[test]
    fn a_compactor_that_was_off_gives_no_warning() {
        let wins = [win(3600., "off"), win(7200., "off")];
        assert_eq!(gate(Some(300_000.), &wins), None);
        // an Off one among live ones is not counted
        let mixed = [win(3600., "off"), win(7200., "auto")];
        assert_eq!(gate(Some(300_000.), &mixed).unwrap().wins, 1);
    }

    #[test]
    fn a_win_older_than_a_week_does_not_count() {
        let wins = [win(WEEK + 60., "auto"), win(8. * 86400., "auto")];
        assert_eq!(gate(Some(300_000.), &wins), None);
        let edge = [win(WEEK - 60., "auto")];
        assert_eq!(gate(Some(300_000.), &edge).unwrap().wins, 1);
    }

    #[test]
    fn an_env_window_names_the_variable_to_unset() {
        let wins = [win(60., "auto")];
        let w = warning(Some(300_000.), source_of(Some("300000")), &wins, NOW).unwrap();
        assert_eq!(
            w.line(),
            "Claude's 300k auto-compact window beat CSR 1\u{d7} this week. \
             Unset CLAUDE_CODE_AUTO_COMPACT_WINDOW to let CSR lead."
        );
        assert_eq!(source_of(Some("junk")), Source::Settings);
        assert_eq!(source_of(None), Source::Settings);
    }

    #[test]
    fn the_ledger_round_trips_and_keeps_a_week() {
        let (_d, paths) = home();
        record_win(&paths, &win(9. * 86400., "auto"));
        record_win(&paths, &win(3600., "auto"));
        record_win(&paths, &win(60., "suggest"));
        let wins = load_wins(&paths);
        assert_eq!(
            wins.len(),
            2,
            "the nine-day-old line left when a newer one was written"
        );
        let settings = json!({"autoCompactWindow": 300000});
        let w = evaluate(&paths, &settings, None, false, NOW).unwrap();
        assert_eq!((w.window, w.wins), (300_000., 2));
    }

    #[test]
    fn the_kill_switch_silences_the_gate() {
        let (_d, paths) = home();
        record_win(&paths, &win(60., "auto"));
        let settings = json!({"autoCompactWindow": 300000});
        assert!(evaluate(&paths, &settings, None, false, NOW).is_some());
        assert!(killed(Some("1")) && killed(Some("true")) && killed(Some("TRUE")));
        assert!(!killed(None) && !killed(Some("0")) && !killed(Some("")));
    }

    #[test]
    fn the_slot_is_one_session_a_day_and_the_pane_shows_it_once() {
        let (_d, paths) = home();
        // nobody holds it: both surfaces may
        assert_eq!(pane_slot(&paths, "a", NOW), PaneSlot::Fresh);
        assert!(recap_slot(&paths, "a", NOW));
        // the recap of a carries the clause; a's pane may still show, b's nothing
        recap_shown(&paths, "a", NOW);
        assert!(!recap_slot(&paths, "a", NOW + 10.), "once in the recap");
        assert_eq!(pane_slot(&paths, "a", NOW + 10.), PaneSlot::Fresh);
        assert_eq!(pane_slot(&paths, "b", NOW + 10.), PaneSlot::No);
        assert!(!recap_slot(&paths, "b", NOW + 10.));
        // a's pane shows: up for the notice's life, then gone and not again
        pane_shown(&paths, "a", NOW + 20.);
        assert_eq!(
            pane_slot(&paths, "a", NOW + 30.),
            PaneSlot::Live {
                until: NOW + 20. + PANE_LIFE
            }
        );
        assert_eq!(pane_slot(&paths, "a", NOW + 20. + PANE_LIFE), PaneSlot::No);
        assert_eq!(pane_slot(&paths, "a", NOW + 3600.), PaneSlot::No);
        // a day later the slot is free again, to anyone
        assert_eq!(pane_slot(&paths, "b", NOW + DAY + 1.), PaneSlot::Fresh);
        assert!(recap_slot(&paths, "b", NOW + DAY + 1.));
        pane_shown(&paths, "b", NOW + DAY + 1.);
        assert_eq!(pane_slot(&paths, "a", NOW + DAY + 2.), PaneSlot::No);
    }

    #[test]
    fn an_unreadable_state_file_leaves_the_slot_free() {
        let (_d, paths) = home();
        fs::write(paths.keep.join(STATE), "not json").unwrap();
        assert_eq!(pane_slot(&paths, "a", NOW), PaneSlot::Fresh);
        assert!(recap_slot(&paths, "a", NOW));
    }

    #[test]
    fn the_recap_line_needs_a_session_and_the_gate() {
        let (_d, paths) = home();
        let settings = json!({"autoCompactWindow": 300000});
        let line = |sid: &str, now: f64| {
            evaluate(&paths, &settings, None, false, now)
                .and_then(|w| recap_line_of(&paths, sid, now, w))
        };
        assert_eq!(line("a", NOW), None, "no win yet");
        record_win(&paths, &win(60., "auto"));
        assert_eq!(line("", NOW), None, "no session id");
        let said = line("a", NOW).unwrap();
        assert!(said.starts_with("Claude's 300k auto-compact window beat CSR 1\u{d7}"));
        recap_shown(&paths, "a", NOW);
        assert_eq!(line("a", NOW + 5.), None, "once per session");
        assert_eq!(line("b", NOW + 5.), None, "once a day");
    }
}
