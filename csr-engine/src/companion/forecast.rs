//! Where the 5H and 7D windows will stand at their reset, from the usage the pane has seen.
//!
//! Every usage reading that reaches the companion is appended to one small local file beside the
//! other companion state, `<keep>/usage-history.jsonl`: one line per window and reading, written
//! only when the figure moved or a few minutes passed. The forecast reads the current window's
//! lines back and extends the recent burn rate to the reset. It says nothing until the window has
//! enough lines spread over enough of its length: a number it cannot back is never shown.

use super::{masthead, private, render::text};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
};

/// The history file under the pane's keep.
pub const FILE: &str = "usage-history.jsonl";

/// The least readings of the current window a forecast stands on.
pub const MIN_SAMPLES: usize = 6;
/// The least share of the window those readings must span.
pub const MIN_SPAN: f64 = 0.10;
/// A window is early until this share of it has passed. While it is early the pace may still
/// swing and the card shows no projected figure, only what earlier windows reached.
pub const EARLY_UNTIL: f64 = 0.10;
/// A reading that did not move is written again after this long, so the file shows the gap.
const REPEAT_AFTER: f64 = 600.;
/// A window's reset time may differ this much between readings and still be the same window.
const SAME_WINDOW: f64 = 600.;
/// Readings older than this are not read back: the longest window and a margin.
const KEEP_FOR: f64 = 9. * 86400.;
/// How much of the file's end is read to find a window's last line.
const TAIL: u64 = 16 * 1024;

/// One reading of one window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    pub at: f64,
    pub used: f64,
    pub resets: f64,
}

/// The two windows the masthead has circles for: (code, API kind, length in seconds).
pub const WINDOWS: [(&str, &str, f64); 2] =
    [("5H", "five_hour", 18000.), ("7D", "seven_day", 604800.)];

fn line_of(kind: &str, s: &Sample) -> String {
    json!({"at": s.at, "kind": kind, "used": s.used, "resets": s.resets}).to_string()
}

fn parse_line(line: &str) -> Option<(String, Sample)> {
    let v: Value = serde_json::from_str(line).ok()?;
    let finite = |k: &str| v[k].as_f64().filter(|n| n.is_finite());
    Some((
        v["kind"].as_str()?.to_owned(),
        Sample {
            at: finite("at")?,
            used: finite("used")?,
            resets: finite("resets")?,
        },
    ))
}

/// The last line the file holds for `kind`, read from its end.
fn last_of(path: &Path, kind: &str) -> Option<Sample> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut text = String::new();
    file.read_to_string(&mut text).ok()?;
    text.lines()
        .rev()
        .filter_map(parse_line)
        .find(|(k, _)| k == kind)
        .map(|(_, s)| s)
}

/// What each process last wrote or saw per file and window, so a steady reading costs no file
/// read at all.
type Memo = HashMap<(PathBuf, String), Sample>;
static MEMO: LazyLock<Mutex<Memo>> = LazyLock::new(|| Mutex::new(Memo::new()));

/// Append the readings of `rate_limits` (the masthead's `{five_hour: {used_percentage,
/// resets_at}}` shape) taken at `at`. A window without both a percentage and a reset time is
/// skipped. A reading equal to the window's last line, or too soon after it to say more, is not
/// written.
pub fn record(keep: &Path, at: f64, rate_limits: &Value) {
    let path = keep.join(FILE);
    let Ok(mut memo) = MEMO.lock() else {
        return;
    };
    for (_, kind, _) in WINDOWS {
        let window = &rate_limits[kind];
        let (Some(used), Some(resets)) = (
            window["used_percentage"]
                .as_f64()
                .filter(|n| n.is_finite() && *n >= 0.),
            window["resets_at"].as_f64().filter(|n| n.is_finite()),
        ) else {
            continue;
        };
        if !at.is_finite() || at <= 0. {
            continue;
        }
        let now = Sample { at, used, resets };
        let key = (path.clone(), kind.to_owned());
        let last = memo.get(&key).copied().or_else(|| last_of(&path, kind));
        if let Some(last) = last {
            let same = last.used == used && (last.resets - resets).abs() < SAME_WINDOW;
            if at <= last.at || (same && at - last.at < REPEAT_AFTER) {
                memo.insert(key, last);
                continue;
            }
        }
        let line = format!("{}\n", line_of(kind, &now));
        if private::append(&path, &line, keep).is_ok() {
            memo.insert(key, now);
        }
    }
}

/// Every line of the file in the last nine days, by window kind. The parse is kept until the file
/// changes, so a frame that asks again reads nothing.
fn history(keep: &Path, now: f64) -> Vec<(String, Sample)> {
    type Hit = (std::time::SystemTime, u64, Vec<(String, Sample)>);
    static CACHE: LazyLock<Mutex<HashMap<PathBuf, Hit>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    let path = keep.join(FILE);
    let Ok(stat) = std::fs::metadata(&path) else {
        return vec![];
    };
    let Ok(modified) = stat.modified() else {
        return vec![];
    };
    let Ok(mut cache) = CACHE.lock() else {
        return vec![];
    };
    if cache
        .get(&path)
        .is_none_or(|hit| hit.0 != modified || hit.1 != stat.len())
    {
        let rows = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter_map(parse_line)
            .collect();
        cache.insert(path.clone(), (modified, stat.len(), rows));
    }
    cache[&path]
        .2
        .iter()
        .filter(|(_, s)| now - s.at < KEEP_FOR)
        .cloned()
        .collect()
}

/// What the windows' own readings say about where one window ends.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Estimate {
    /// The percentage the window reaches at its reset, not capped: above 100 means it fills.
    pub at_reset: f64,
    /// When the window fills before its reset, when it does.
    pub runs_out: Option<f64>,
    /// Less than a tenth of the window has passed: the figure may still swing. The card shows no
    /// figure for an early window (see `outlook`); the math is kept for the later ones.
    pub early: bool,
    /// Finished windows' peaks were blended in, not the live pace alone.
    pub blended: bool,
}

/// Where a window ends up. The live pace is the share used over the share of the window gone;
/// it stands once two readings of this window exist or a twentieth of the window has passed.
/// With finished windows behind it, the median of their peaks is blended in, more of it the
/// earlier in the window it is. `None` when neither backs a figure: nothing is made up.
/// `seen` is how many readings of this window exist, the newest included.
pub fn estimate(
    len: f64,
    used: f64,
    resets: f64,
    now: f64,
    seen: usize,
    peaks: &[f64],
) -> Option<Estimate> {
    let left = resets - now;
    if !(left > 0. && left <= len && used.is_finite() && (0. ..=100.).contains(&used)) {
        return None;
    }
    let elapsed = 1. - left / len;
    let live = (elapsed >= 0.01 && (seen >= 2 || elapsed >= 0.05)).then(|| used / elapsed);
    let typical = median(peaks);
    let (reach, blended) = match (live, typical) {
        (Some(live), Some(typical)) => {
            let weight = (elapsed / 0.5).clamp(0.2, 1.);
            (weight * live + (1. - weight) * typical, true)
        }
        (Some(live), None) => (live, false),
        (None, Some(typical)) => (typical, true),
        (None, None) => return None,
    };
    let reach = reach.max(used);
    let runs_out = (reach >= 100. && used < 100.)
        .then(|| now + (100. - used) / (reach - used) * left)
        .filter(|at| *at < resets);
    Some(Estimate {
        at_reset: reach,
        runs_out,
        early: is_early(elapsed),
        blended,
    })
}

/// Whether `elapsed` (the share of the window gone) is under `EARLY_UNTIL`. A hair of slack so a
/// tenth that float division lands on 0.0999999999 still counts as a tenth.
fn is_early(elapsed: f64) -> bool {
    elapsed < EARLY_UNTIL - 1e-9
}

fn median(values: &[f64]) -> Option<f64> {
    let mut v: Vec<f64> = values.iter().copied().filter(|n| n.is_finite()).collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let mid = v.len() / 2;
    Some(if v.len() % 2 == 1 {
        v[mid]
    } else {
        (v[mid - 1] + v[mid]) / 2.
    })
}

/// The highest figure each finished window of `kind` reached, from its readings, where the
/// readings reach the window's last fifth (a window seen only at its start says nothing of its
/// end). The current window, the one that resets at `resets`, is not one of them. Each peak
/// comes with the reset time of its window, oldest first.
fn finished(
    rows: &[(String, Sample)],
    kind: &str,
    len: f64,
    resets: f64,
    now: f64,
) -> Vec<(f64, f64)> {
    let mut mine: Vec<Sample> = rows
        .iter()
        .filter(|(k, s)| k == kind && (s.resets - resets).abs() >= SAME_WINDOW && s.resets < now)
        .map(|(_, s)| *s)
        .collect();
    mine.sort_by(|a, b| a.resets.total_cmp(&b.resets));
    let mut out = vec![];
    let mut group: Vec<Sample> = vec![];
    let mut close = |group: &mut Vec<Sample>| {
        if let Some(first) = group.first() {
            let end = group.iter().map(|s| s.at).fold(f64::MIN, f64::max);
            if end >= first.resets - len * 0.2 {
                out.push((
                    first.resets,
                    group.iter().map(|s| s.used).fold(0., f64::max),
                ));
            }
        }
        group.clear();
    };
    for s in mine {
        if group
            .first()
            .is_some_and(|g| (s.resets - g.resets).abs() >= SAME_WINDOW)
        {
            close(&mut group);
        }
        group.push(s);
    }
    close(&mut group);
    out
}

/// The words under a circle of the old layouts: where the window ends up, or roughly when it
/// fills.
pub fn caption(f: &Estimate, now: f64) -> (String, bool) {
    match f.runs_out {
        Some(at) => (format!("out ~{}", masthead::reset_label(at, now)), true),
        None => (
            format!("→ ~{:.0}% at reset", f.at_reset.min(100.).round()),
            false,
        ),
    }
}

/// A span left, as "4h 35m", "23m" or "2d 20h".
pub fn until(seconds: f64) -> String {
    let s = seconds.max(0.);
    let (d, h, m) = (
        (s / 86400.).floor() as i64,
        ((s % 86400.) / 3600.).floor() as i64,
        ((s % 3600.) / 60.).floor() as i64,
    );
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m:02}m")
    } else {
        format!("{m}m")
    }
}

/// A moment as the owner reads a clock: "3:40 pm", or "Thu 3:40 pm" on another day.
pub fn clock(at: f64, now: f64) -> String {
    use chrono::{Local, TimeZone};
    let Some(when) = Local.timestamp_opt(at as i64, 0).single() else {
        return String::new();
    };
    let same_day = Local
        .timestamp_opt(now as i64, 0)
        .single()
        .is_some_and(|n| n.date_naive() == when.date_naive());
    when.format(if same_day {
        "%-I:%M %P"
    } else {
        "%a %-I:%M %P"
    })
    .to_string()
}

/// "Thursday's reset", or "the 3:40 pm reset" for a reset later today.
fn reset_name(at: f64, now: f64) -> String {
    use chrono::{Local, TimeZone};
    let Some(when) = Local.timestamp_opt(at as i64, 0).single() else {
        return "the reset".into();
    };
    let same_day = Local
        .timestamp_opt(now as i64, 0)
        .single()
        .is_some_and(|n| n.date_naive() == when.date_naive());
    if same_day {
        format!("the {} reset", when.format("%-I:%M %P"))
    } else {
        format!("{}'s reset", when.format("%A"))
    }
}

/// What the finished windows before this one reached: the median of their peaks.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Past {
    median: f64,
    /// How many finished windows the median stands on.
    count: usize,
    /// Exactly one, and it ended just before this one began: it is "last week", not "a week".
    last: bool,
}

impl Past {
    /// `done` is `finished()`'s output for the window that resets at `resets`.
    fn of(done: &[(f64, f64)], len: f64, resets: f64) -> Option<Past> {
        let median = median(&done.iter().map(|d| d.1).collect::<Vec<_>>())?;
        Some(Past {
            median,
            count: done.len(),
            last: done.len() == 1 && resets - done[0].0 < 1.5 * len,
        })
    }

    /// "last week peaked at 93%": which window it was, and the figure it reached.
    fn words(&self, code: &str) -> String {
        let peak = self.median.round();
        match (code == "7D", self.count, self.last) {
            (true, _, _) if self.count > 1 => format!("earlier weeks peaked around {peak}%"),
            (true, _, true) => format!("last week peaked at {peak}%"),
            (true, _, false) => format!("an earlier week peaked at {peak}%"),
            (false, _, _) if self.count > 1 => {
                format!("earlier 5-hour windows peaked around {peak}%")
            }
            (false, _, true) => format!("the last 5-hour window peaked at {peak}%"),
            (false, _, false) => format!("an earlier 5-hour window peaked at {peak}%"),
        }
    }
}

/// One window as the card says it.
struct Row {
    code: &'static str,
    name: &'static str,
    used: f64,
    resets: f64,
    /// The figure the card stands on. `None` while the window is early: nothing is projected.
    estimate: Option<Estimate>,
    early: bool,
    past: Option<Past>,
}

impl Row {
    /// The colour of its state by what it has used and where it is headed: amber from 75%, red
    /// from 90% or when it fills before its reset.
    fn sev(&self) -> &'static str {
        let reach = self
            .estimate
            .map_or(self.used, |e| e.at_reset.max(self.used));
        if self.used >= 100. || reach >= 90. || self.estimate.is_some_and(|e| e.runs_out.is_some())
        {
            "red"
        } else if reach >= 75. {
            "amber"
        } else {
            "ok"
        }
    }

    fn line(&self, now: f64) -> String {
        let left = until(self.resets - now);
        if self.used >= 100. {
            return format!("Limit reached · resets in {left}");
        }
        // Early in the window there is no pace to state: say what earlier windows reached, or
        // that it is too soon. A prior is never worded as a measured pace.
        if self.early {
            return match self.past {
                Some(past) => format!("Early · {} · resets in {left}", past.words(self.code)),
                None => format!("Too early to tell · resets in {left}"),
            };
        }
        let Some(e) = self.estimate else {
            return format!("Pace shows after a few readings · resets in {left}");
        };
        match e.runs_out {
            Some(at) => format!(
                "On pace to run out around {} · resets in {left}",
                clock(at, now)
            ),
            None => format!(
                "On pace for about {:.0}% by reset · resets in {left}",
                e.at_reset.round()
            ),
        }
    }
}

/// The sentence that heads the card and the colour it is in. It stands on the windows that are
/// past their early stretch; an early window adds only what it has already used, never a guess.
fn headline(rows: &[Row], now: f64) -> Option<(String, &'static str)> {
    let first = rows.first()?;
    let soonest = rows
        .iter()
        .filter_map(|r| Some((r, r.estimate?.runs_out?)))
        .min_by(|a, b| a.1.total_cmp(&b.1));
    if let Some((row, at)) = soonest {
        return Some((
            format!(
                "On course to run out: {} limit hit around {}.",
                row.name,
                clock(at, now)
            ),
            "red",
        ));
    }
    if let Some(row) = rows.iter().find(|r| r.used >= 100.) {
        return Some((
            format!(
                "{} limit reached. It resets in {}.",
                row.name,
                until(row.resets - now)
            ),
            "red",
        ));
    }
    let reach = |r: &Row| r.estimate.map_or(r.used, |e| e.at_reset.max(r.used));
    let worst = rows
        .iter()
        .filter(|r| !r.early)
        .max_by(|a, b| reach(a).total_cmp(&reach(b)));
    let at = worst.map_or(0., reach);
    // An early window far into its limit already says so, in what it has used.
    let used_up = rows
        .iter()
        .filter(|r| r.early && r.used >= 75. && r.used > at)
        .max_by(|a, b| a.used.total_cmp(&b.used));
    if let Some(row) = used_up {
        return Some((
            format!(
                "{} is already at {:.0}% of its limit.",
                row.name,
                row.used.round()
            ),
            if row.used >= 90. { "red" } else { "amber" },
        ));
    }
    if let Some(worst) = worst {
        if at >= 90. {
            return Some((
                format!(
                    "Close to the limit. Expect about {:.0}% of the {} limit by reset.",
                    at.round(),
                    worst.name
                ),
                "red",
            ));
        }
        if at >= 75. {
            return Some((
                format!(
                    "Heading toward the limit: about {:.0}% of the {} limit by reset.",
                    at.round(),
                    worst.name
                ),
                "amber",
            ));
        }
    }
    if rows.iter().all(|r| r.estimate.is_none()) {
        // Every window is early, or has no pace yet: no verdict, only where it stands.
        if rows.iter().all(|r| r.early) {
            let lead = rows.iter().find(|r| r.code == "7D").unwrap_or(first);
            let span = if lead.code == "7D" { "week" } else { "window" };
            return Some((
                format!(
                    "Early in the {span}. {} is at {:.0}% so far.",
                    lead.name,
                    lead.used.round()
                ),
                "quiet",
            ));
        }
        return Some((
            format!(
                "Watching the limits: {} is at {:.0}% so far.",
                first.name,
                first.used.round()
            ),
            "quiet",
        ));
    }
    // The reset it names is the week's, as the usage mod does; the 5 Hour's when the week is
    // early or absent.
    let lead = rows
        .iter()
        .find(|r| r.code == "7D" && r.estimate.is_some())
        .or(worst)?;
    Some((
        format!(
            "On track. You should reach {} with room to spare.",
            reset_name(lead.resets, now)
        ),
        "ok",
    ))
}

/// The card's outlook for `rate_limits` (the masthead's shape) at `now`: a headline, one line
/// per window with its tick, and, for the layouts that still draw circles, the short caption
/// under each as `{"5H": {"text", "out"}}` in `quota`. A window with no readings is not there.
pub fn outlook(keep: &Path, rate_limits: &Value, now: f64) -> Value {
    let rows = history(keep, now);
    let mut windows = vec![];
    for (code, kind, len) in WINDOWS {
        let window = &rate_limits[kind];
        let (Some(used), Some(resets)) = (
            window["used_percentage"]
                .as_f64()
                .filter(|n| n.is_finite() && *n >= 0.),
            window["resets_at"].as_f64().filter(|n| n.is_finite()),
        ) else {
            continue;
        };
        if resets <= now || resets - now > len {
            continue;
        }
        // Readings of this window by the second they were taken: two panes writing the same
        // reading are one reading.
        let seen = rows
            .iter()
            .filter(|(k, s)| {
                k == kind && (s.resets - resets).abs() < SAME_WINDOW && s.at <= now + 60.
            })
            .map(|(_, s)| s.at.floor() as i64)
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        let done = finished(&rows, kind, len, resets, now);
        let peaks: Vec<f64> = done.iter().map(|d| d.1).collect();
        let early = is_early(1. - (resets - now) / len);
        windows.push(Row {
            code,
            name: if code == "5H" { "5 Hour" } else { "Weekly" },
            used,
            resets,
            // The math runs for every window, but an early one shows no figure from it.
            estimate: estimate(len, used, resets, now, seen, &peaks).filter(|_| !early),
            early,
            past: Past::of(&done, len, resets),
        });
    }
    let mut quota = serde_json::Map::new();
    for r in &windows {
        if let Some(e) = r.estimate {
            let (text, out) = caption(&e, now);
            quota.insert(
                r.code.into(),
                json!({"text": text::one_line(&text, 40), "out": out}),
            );
        }
    }
    let head = headline(&windows, now);
    json!({
        "headline": head.map(|(text, sev)| json!({"text": text, "sev": sev})),
        "windows": windows.iter().map(|r| json!({
            "code": r.code,
            "name": r.name,
            "used": r.used,
            "sev": r.sev(),
            "tick": r.estimate.map(|e| e.at_reset.clamp(0., 100.).round()),
            "early": r.early,
            "blended": r.estimate.is_some_and(|e| e.blended),
            "line": r.line(now),
        })).collect::<Vec<_>>(),
        "quota": quota,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEN: f64 = 18000.;
    const RESETS: f64 = 1_800_018_000.;

    /// The moment `share` of the window has gone.
    fn at(share: f64) -> f64 {
        RESETS - LEN + share * LEN
    }

    #[test]
    fn test_the_live_pace_is_the_share_used_over_the_share_gone() {
        // A fifth of the window gone, 10 points used: 50 points at the reset.
        let e = estimate(LEN, 10., RESETS, at(0.2), 1, &[]).unwrap();
        assert!((e.at_reset - 50.).abs() < 1e-9, "{e:?}");
        assert!(!e.early && !e.blended && e.runs_out.is_none());
        // Early until a tenth of the window has passed.
        assert!(estimate(LEN, 3., RESETS, at(0.06), 1, &[]).unwrap().early);
        // One reading, a hundredth gone: too soon for a pace, and no prior is made up.
        assert_eq!(estimate(LEN, 1., RESETS, at(0.03), 1, &[]), None);
        // Two readings stand for it earlier.
        let two = estimate(LEN, 1., RESETS, at(0.03), 2, &[]).unwrap();
        assert!((two.at_reset - 33.33).abs() < 0.01, "{two:?}");
        assert_eq!(estimate(LEN, 1., RESETS, at(0.005), 9, &[]), None);
        // A window that is over, or not yet open, has none.
        assert_eq!(estimate(LEN, 10., RESETS, RESETS + 1., 9, &[]), None);
        assert_eq!(estimate(LEN, 10., RESETS, RESETS - 2. * LEN, 9, &[]), None);
    }

    #[test]
    fn test_a_pace_that_fills_the_window_names_when() {
        // Half the window gone, 80 points used: 160 at the reset, full at 5/8 of the window.
        let now = at(0.5);
        let e = estimate(LEN, 80., RESETS, now, 5, &[]).unwrap();
        assert_eq!(e.at_reset, 160.);
        let out = e.runs_out.expect("fills before the reset");
        assert!((out - at(0.625)).abs() < 1e-6, "{out}");
        // Used up already: no future time to name.
        assert_eq!(
            estimate(LEN, 100., RESETS, now, 5, &[]).unwrap().runs_out,
            None
        );
        let (text, hot) = caption(&e, now);
        assert!(hot && text.starts_with("out ~"), "{text}");
    }

    #[test]
    fn test_finished_windows_pull_an_early_pace_toward_their_median() {
        // A fifth gone, 10 used: live says 50. Past peaks 20 and 40: median 30.
        let live = estimate(LEN, 10., RESETS, at(0.2), 5, &[]).unwrap();
        let mixed = estimate(LEN, 10., RESETS, at(0.2), 5, &[20., 40.]).unwrap();
        assert!(mixed.blended && !live.blended);
        // Weight 0.4 on live: 0.4 * 50 + 0.6 * 30 = 38.
        assert!((mixed.at_reset - 38.).abs() < 1e-9, "{mixed:?}");
        // Late in the window the live pace counts alone.
        let late = estimate(LEN, 60., RESETS, at(0.6), 5, &[20., 40.]).unwrap();
        assert!((late.at_reset - 100.).abs() < 1e-9, "{late:?}");
        // With one reading only and history behind it, the typical window stands.
        let typical = estimate(LEN, 1., RESETS, at(0.005), 1, &[20., 40.]).unwrap();
        assert!((typical.at_reset - 30.).abs() < 1e-9 && typical.early);
        // The figure never reads below what is already used.
        assert_eq!(
            estimate(LEN, 90., RESETS, at(0.9), 5, &[10.])
                .unwrap()
                .at_reset,
            100.
        );
    }

    fn rows_of(kind: &str, windows: &[(f64, &[(f64, f64)])]) -> Vec<(String, Sample)> {
        let mut out = vec![];
        for (resets, readings) in windows {
            for (share, used) in *readings {
                out.push((
                    kind.to_owned(),
                    Sample {
                        at: resets - LEN + share * LEN,
                        used: *used,
                        resets: *resets,
                    },
                ));
            }
        }
        out
    }

    #[test]
    fn test_only_finished_windows_seen_to_their_end_give_a_peak() {
        let old = RESETS - 3. * LEN;
        let thin = RESETS - 2. * LEN;
        let rows = rows_of(
            "five_hour",
            &[
                (old, &[(0.2, 10.), (0.9, 42.), (0.95, 40.)]),
                // Seen only at its start: its peak says nothing of its end.
                (thin, &[(0.1, 5.), (0.3, 9.)]),
                // The current window never counts.
                (RESETS, &[(0.5, 70.), (0.9, 99.)]),
            ],
        );
        assert_eq!(
            finished(&rows, "five_hour", LEN, RESETS, RESETS - 600.),
            vec![(old, 42.)]
        );
        // Other kinds are not mixed in.
        assert!(finished(&rows, "seven_day", LEN, RESETS, RESETS - 600.).is_empty());
    }

    fn limits(five: f64, week: f64, now: f64) -> Value {
        limits_left(five, week, now, 4. * 3600. + 35. * 60.)
    }

    fn limits_left(five: f64, week: f64, now: f64, left: f64) -> Value {
        json!({"five_hour": {"used_percentage": five, "resets_at": now + left},
               "seven_day": {"used_percentage": week, "resets_at": now + 2. * 86400. + 20. * 3600.}})
    }

    #[test]
    fn test_the_outlook_speaks_from_the_first_reading_and_names_the_headline() {
        let d = tempfile::tempdir().unwrap();
        let keep = d.path().join("csr-companion");
        // No history file at all.
        let now = 1_800_000_000.;
        let got = outlook(&keep, &limits(8., 20., now), now);
        let w = got["windows"].as_array().unwrap();
        assert_eq!(w.len(), 2);
        assert_eq!(w[0]["code"], "5H");
        let line = w[0]["line"].as_str().unwrap();
        // 25 minutes of 300 gone: under a tenth, so no pace is stated, however high it reads.
        assert_eq!(line, "Too early to tell · resets in 4h 35m");
        assert!(w[0]["tick"].is_null() && w[0]["early"] == true);
        // The week: 2d20h left of 7d is 59% gone, 20 used: 34%.
        let week = w[1]["line"].as_str().unwrap();
        assert!(
            week.starts_with("On pace for about 34% by reset · resets in 2d 20h"),
            "{week}"
        );
        assert!(w[1]["tick"].as_f64().unwrap() > 30. && w[1]["early"] == false);
        // The headline rests on the week alone.
        assert_eq!(got["headline"]["sev"], "ok", "{got}");
        assert!(got["headline"]["text"]
            .as_str()
            .unwrap()
            .starts_with("On track. You should reach "));
        // No caption under the early window's circle.
        assert!(got["quota"]["5H"].is_null(), "{got}");
        assert_eq!(got["quota"]["7D"]["out"], false);
    }

    #[test]
    fn test_the_headline_has_one_state_for_each_way_a_window_can_end() {
        let d = tempfile::tempdir().unwrap();
        let keep = d.path().join("csr-companion");
        let now = 1_800_000_000.;
        // Two hours of five left. On track: little used, well on the way.
        let two_hours = |five: f64| limits_left(five, 10., now, 7200.);
        let calm = outlook(&keep, &two_hours(12.), now);
        assert_eq!(calm["headline"]["sev"], "ok", "{calm}");
        let text = calm["headline"]["text"].as_str().unwrap();
        assert!(
            text.starts_with("On track") && text.contains("room to spare"),
            "{text}"
        );
        // Amber from 75% at the reset: 48 used with 60% gone is 80.
        let mid = outlook(&keep, &two_hours(48.), now);
        assert_eq!(mid["headline"]["sev"], "amber", "{mid}");
        assert!(mid["headline"]["text"].as_str().unwrap().contains("5 Hour"));
        // Red from 90%: 55 used is 92 at the reset.
        let near = outlook(&keep, &two_hours(55.), now);
        assert_eq!(near["headline"]["sev"], "red", "{near}");
        assert!(near["headline"]["text"]
            .as_str()
            .unwrap()
            .starts_with("Close to the limit"));
        assert_eq!(near["quota"]["5H"]["out"], false);
        // Out: the pace fills the window before its reset, so the headline says when.
        let out = outlook(&keep, &two_hours(70.), now);
        assert_eq!(out["headline"]["sev"], "red");
        let text = out["headline"]["text"].as_str().unwrap();
        assert!(
            text.starts_with("On course to run out: 5 Hour limit hit around "),
            "{text}"
        );
        assert_eq!(out["quota"]["5H"]["out"], true);
        // Used up: said plainly.
        let full = outlook(&keep, &two_hours(100.), now);
        assert!(full["headline"]["text"]
            .as_str()
            .unwrap()
            .starts_with("5 Hour limit reached"));
        // No limits, no headline and no window.
        let none = outlook(&keep, &json!({}), now);
        assert_eq!(none["headline"], Value::Null);
        assert_eq!(none["windows"], json!([]));
        // Too soon in a window for any pace: it says so, with no number invented.
        let soon = json!({"five_hour": {"used_percentage": 1., "resets_at": now + 17900.}});
        let got = outlook(&keep, &soon, now);
        assert_eq!(got["headline"]["sev"], "quiet", "{got}");
        assert_eq!(
            got["headline"]["text"],
            "Early in the window. 5 Hour is at 1% so far."
        );
        assert!(got["windows"][0]["tick"].is_null());
        assert!(got["windows"][0]["line"]
            .as_str()
            .unwrap()
            .starts_with("Too early to tell"));
    }

    #[test]
    fn test_the_on_track_headline_names_the_weeks_reset_and_ignores_an_early_window() {
        let d = tempfile::tempdir().unwrap();
        let keep = d.path().join("csr-companion");
        let now = 1_800_000_000.;
        // Two hours of five left, little used: the sentence names the week's reset, not the
        // 5 Hour's.
        let calm = outlook(&keep, &limits_left(12., 10., now, 7200.), now);
        let text = calm["headline"]["text"].as_str().unwrap();
        let week = reset_name(now + 2. * 86400. + 20. * 3600., now);
        assert_eq!(
            text,
            format!("On track. You should reach {week} with room to spare.")
        );
        assert!(week.ends_with("'s reset"), "{week}");
        // The 5 Hour is early (a twentieth gone): it adds nothing, and the sentence is the same
        // with no early mark, resting on the week.
        let early = outlook(&keep, &limits_left(1., 10., now, 17000.), now);
        assert_eq!(early["headline"]["text"], calm["headline"]["text"]);
        assert_eq!(early["headline"]["sev"], "ok");
        // An early 5 Hour that reads as a fast pace does not turn the headline red or amber.
        let fast = outlook(&keep, &limits_left(30., 10., now, 17100.), now);
        assert_eq!(fast["headline"]["text"], calm["headline"]["text"], "{fast}");
        assert!(fast["windows"][0]["tick"].is_null());
        assert_eq!(fast["quota"]["5H"], Value::Null);
    }

    #[test]
    fn test_an_early_week_with_a_calm_five_hour_names_the_five_hour_reset() {
        let d = tempfile::tempdir().unwrap();
        let keep = d.path().join("csr-companion");
        let now = 1_800_000_000.;
        // The week is 1% into its first day; the 5 Hour is two hours in with 12% used.
        let limits = json!({
            "five_hour": {"used_percentage": 12., "resets_at": now + 7200.},
            "seven_day": {"used_percentage": 1., "resets_at": now + 0.9831 * 604800.}});
        let got = outlook(&keep, &limits, now);
        assert_eq!(
            got["headline"]["text"],
            format!(
                "On track. You should reach {} with room to spare.",
                reset_name(now + 7200., now)
            ),
            "{got}"
        );
        assert_eq!(got["windows"][1]["early"], true);
        assert!(got["windows"][1]["tick"].is_null());
    }

    #[test]
    fn test_the_headline_when_every_window_is_early_is_neutral() {
        let d = tempfile::tempdir().unwrap();
        let keep = d.path().join("csr-companion");
        let now = 1_800_000_000.;
        // The live case of 2026-11-01: week 1% used, 1.7% gone; the 5 Hour just opened.
        let limits = json!({
            "five_hour": {"used_percentage": 2., "resets_at": now + 17000.},
            "seven_day": {"used_percentage": 1., "resets_at": now + 0.9831 * 604800.}});
        let got = outlook(&keep, &limits, now);
        assert_eq!(
            got["headline"]["text"],
            "Early in the week. Weekly is at 1% so far."
        );
        assert_eq!(got["headline"]["sev"], "quiet");
        // Not on track, not running out, and no number projected.
        let said = got.to_string();
        assert!(
            !said.contains("On track") && !said.contains("run out"),
            "{said}"
        );
        assert!(
            !said.contains("by reset") && !said.contains("at reset"),
            "{said}"
        );
        // Much used already, early: it says what is used, not what it may reach.
        let hot = json!({"five_hour": {"used_percentage": 80., "resets_at": now + 17000.}});
        let got = outlook(&keep, &hot, now);
        assert_eq!(
            got["headline"]["text"],
            "5 Hour is already at 80% of its limit."
        );
        assert_eq!(got["headline"]["sev"], "amber");
        assert_eq!(got["windows"][0]["sev"], "amber");
    }

    /// One finished window of `kind`: readings through its last fifth that peak at `peak`.
    fn finished_window(kind: &str, len: f64, resets: f64, peak: f64) -> String {
        [(0.4, peak / 2.), (0.9, peak), (0.97, peak - 1.)]
            .iter()
            .map(|(share, used)| {
                format!(
                    "{}\n",
                    line_of(
                        kind,
                        &Sample {
                            at: resets - len + share * len,
                            used: *used,
                            resets,
                        }
                    )
                )
            })
            .collect()
    }

    #[test]
    fn test_an_early_window_with_history_says_what_the_last_one_peaked_at() {
        let d = tempfile::tempdir().unwrap();
        let keep = d.path().join("csr-companion");
        std::fs::create_dir_all(&keep).unwrap();
        let now = 1_800_000_000.;
        // The live case: the week is 1.69% gone with 1% used (a pace of 59%), and last week
        // peaked at 93%. The 5 Hour opened 17000 s from its reset; the one before peaked at 40%.
        let week = now + 0.9831 * 604800.;
        let five = now + 17000.;
        let body = [
            finished_window("seven_day", 604800., week - 604800., 93.),
            finished_window("five_hour", 18000., five - 18000., 40.),
        ]
        .concat();
        std::fs::write(keep.join(FILE), body).unwrap();
        let limits = json!({
            "five_hour": {"used_percentage": 2., "resets_at": five},
            "seven_day": {"used_percentage": 1., "resets_at": week}});
        let got = outlook(&keep, &limits, now);
        let w = got["windows"].as_array().unwrap();
        assert_eq!(
            w[0]["line"],
            format!(
                "Early · the last 5-hour window peaked at 40% · resets in {}",
                until(five - now)
            )
        );
        assert_eq!(
            w[1]["line"],
            format!(
                "Early · last week peaked at 93% · resets in {}",
                until(week - now)
            )
        );
        // The prior is not worded as a pace and not drawn as a tick, a caption or a headline.
        assert!(w
            .iter()
            .all(|r| r["tick"].is_null() && r["blended"] == false));
        assert_eq!(got["quota"], json!({}));
        assert_eq!(
            got["headline"]["text"],
            "Early in the week. Weekly is at 1% so far."
        );
        assert!(!got.to_string().contains("86"), "{got}");
        // A week skipped in between: it is no longer "last week".
        std::fs::write(
            keep.join(FILE),
            finished_window("seven_day", 604800., week - 2. * 604800., 93.),
        )
        .unwrap();
        let got = outlook(&keep, &limits, now);
        assert!(got["windows"][1]["line"]
            .as_str()
            .unwrap()
            .starts_with("Early · an earlier week peaked at 93% ·"));
    }

    #[test]
    fn test_several_finished_windows_are_worded_as_the_median_they_are() {
        let past = |peaks: &[(f64, f64)]| Past::of(peaks, LEN, RESETS).unwrap();
        // Two peaks: 30 and 60. The figure is their median, 45, and the words say "around".
        let two = past(&[(RESETS - 2. * LEN, 30.), (RESETS - LEN, 60.)]);
        assert_eq!(two.median, 45.);
        assert_eq!(two.words("7D"), "earlier weeks peaked around 45%");
        assert_eq!(two.words("5H"), "earlier 5-hour windows peaked around 45%");
        let one = past(&[(RESETS - LEN, 93.4)]);
        assert_eq!(one.words("7D"), "last week peaked at 93%");
        assert_eq!(one.words("5H"), "the last 5-hour window peaked at 93%");
        assert_eq!(Past::of(&[], LEN, RESETS), None);
    }

    #[test]
    fn test_a_window_is_early_until_a_tenth_of_it_has_gone() {
        let d = tempfile::tempdir().unwrap();
        let keep = d.path().join("csr-companion");
        let now = 1_800_000_000.;
        for len in [18000., 604800.] {
            let code = if len == 18000. { "5H" } else { "7D" };
            let kind = if len == 18000. {
                "five_hour"
            } else {
                "seven_day"
            };
            let at = |left: f64| {
                let limits = json!({kind: {"used_percentage": 5., "resets_at": now + left}});
                outlook(&keep, &limits, now)["windows"][0].clone()
            };
            // Exactly a tenth gone: the pace stands (5 used over a tenth: 50 at the reset).
            let tenth = at(0.9 * len);
            assert_eq!(tenth["code"], code);
            assert_eq!(tenth["early"], false, "{code}: {tenth}");
            assert_eq!(tenth["tick"], 50., "{code}: {tenth}");
            assert!(
                tenth["line"]
                    .as_str()
                    .unwrap()
                    .starts_with("On pace for about 50% by reset"),
                "{code}: {tenth}"
            );
            assert!(!tenth["line"].as_str().unwrap().contains("Early"));
            // A second short of it: still early, nothing projected.
            let before = at(0.9 * len + 1.);
            assert_eq!(before["early"], true, "{code}: {before}");
            assert!(before["tick"].is_null(), "{code}: {before}");
            assert!(before["line"]
                .as_str()
                .unwrap()
                .starts_with("Too early to tell"));
        }
    }

    #[test]
    fn test_a_finished_window_in_the_history_blends_into_the_outlook() {
        let d = tempfile::tempdir().unwrap();
        let keep = d.path().join("csr-companion");
        std::fs::create_dir_all(&keep).unwrap();
        let now = 1_800_000_000.;
        // A fifth of the window gone, past its early stretch: 8 used is 40 at the reset alone.
        let resets = now + 4. * 3600.;
        let old = resets - 2. * LEN;
        let body: String = [(0.3, 10.), (0.9, 20.), (0.97, 20.)]
            .iter()
            .map(|(share, used)| {
                format!(
                    "{}\n",
                    line_of(
                        "five_hour",
                        &Sample {
                            at: old - LEN + share * LEN,
                            used: *used,
                            resets: old,
                        }
                    )
                )
            })
            .collect();
        std::fs::write(keep.join(FILE), body).unwrap();
        let limits = limits_left(8., 20., now, 4. * 3600.);
        let alone = outlook(&tempfile::tempdir().unwrap().path().join("x"), &limits, now);
        let got = outlook(&keep, &limits, now);
        assert_eq!(got["windows"][0]["blended"], true);
        assert_eq!(alone["windows"][0]["blended"], false);
        // The past peak of 20 pulls the 40% pace down.
        assert!(
            got["windows"][0]["tick"].as_f64().unwrap()
                < alone["windows"][0]["tick"].as_f64().unwrap()
        );
        // Early in the same window, the same history shows as a peak, not as a figure.
        let early = outlook(
            &keep,
            &limits_left(8., 20., now, 4. * 3600. + 35. * 60.),
            now,
        );
        assert_eq!(early["windows"][0]["blended"], false);
        assert!(early["windows"][0]["tick"].is_null());
        assert!(early["windows"][0]["line"]
            .as_str()
            .unwrap()
            .starts_with("Early · an earlier 5-hour window peaked at 20% ·"));
    }

    #[test]
    fn test_readings_of_an_early_window_never_make_a_pace_on_the_card() {
        let d = tempfile::tempdir().unwrap();
        let keep = d.path().join("csr-companion");
        std::fs::create_dir_all(&keep).unwrap();
        // 3% of the window gone. Two readings would be enough for `estimate` to state a pace;
        // the card still shows none while the window is early, one reading or several.
        let now = 1_800_000_000.;
        let resets = now + 0.97 * LEN;
        let one = Sample {
            at: now - 30.,
            used: 2.,
            resets,
        };
        let limits = json!({"five_hour": {"used_percentage": 2., "resets_at": resets}});
        let twice = format!("{0}\n{0}\n", line_of("five_hour", &one));
        std::fs::write(keep.join(FILE), twice).unwrap();
        let said = |keep: &Path| {
            let got = outlook(keep, &limits, now);
            assert!(got["windows"][0]["tick"].is_null(), "{got}");
            got["windows"][0]["line"].as_str().unwrap().to_owned()
        };
        assert!(said(&keep).starts_with("Too early to tell"));
        let more = Sample {
            at: now - 90.,
            ..one
        };
        let both = format!(
            "{}\n{}\n",
            line_of("five_hour", &more),
            line_of("five_hour", &one)
        );
        std::fs::write(keep.join(FILE), both).unwrap();
        assert!(said(&keep).starts_with("Too early to tell"));
    }

    #[test]
    fn test_spans_and_clocks_read_the_way_a_person_says_them() {
        assert_eq!(until(4. * 3600. + 35. * 60.), "4h 35m");
        assert_eq!(until(23. * 60.), "23m");
        assert_eq!(until(2. * 86400. + 20. * 3600.), "2d 20h");
        assert_eq!(until(-5.), "0m");
    }

    #[test]
    fn test_readings_are_appended_once_while_the_figure_stands_still() {
        let d = tempfile::tempdir().unwrap();
        let keep = d.path().join("csr-companion");
        let limits = |used: f64| {
            json!({"five_hour": {"used_percentage": used, "resets_at": 1_800_018_000.},
                   "seven_day": {"used_percentage": 40., "resets_at": 1_800_500_000.}})
        };
        let lines = |keep: &Path| {
            std::fs::read_to_string(keep.join(FILE))
                .unwrap_or_default()
                .lines()
                .count()
        };
        record(&keep, 1_800_000_000., &limits(10.));
        assert_eq!(lines(&keep), 2);
        // The same figures a minute later: nothing new.
        record(&keep, 1_800_000_060., &limits(10.));
        assert_eq!(lines(&keep), 2);
        // A moved figure is written, the other window is not.
        record(&keep, 1_800_000_120., &limits(11.));
        assert_eq!(lines(&keep), 3);
        // The same figure after ten minutes is written again, so the gap shows.
        // (The week's figure also stood still for ten minutes, so it is written too.)
        record(&keep, 1_800_000_900., &limits(11.));
        assert_eq!(lines(&keep), 5);
        // An older reading never goes in after a newer one.
        record(&keep, 1_800_000_100., &limits(12.));
        assert_eq!(lines(&keep), 5);
        // A window with no percentage or reset is skipped.
        record(
            &keep,
            1_800_001_000.,
            &json!({"five_hour": {"used_percentage": 3.}}),
        );
        assert_eq!(lines(&keep), 5);
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(keep.join(FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn test_a_new_process_does_not_repeat_what_the_file_already_ends_with() {
        let d = tempfile::tempdir().unwrap();
        let keep = d.path().join("csr-companion");
        std::fs::create_dir_all(&keep).unwrap();
        let resets = 1_800_018_000.;
        let seen = Sample {
            at: 1_800_000_000.,
            used: 10.,
            resets,
        };
        std::fs::write(
            keep.join(FILE),
            format!("{}\n", line_of("five_hour", &seen)),
        )
        .unwrap();
        let limits = json!({"five_hour": {"used_percentage": 10., "resets_at": resets}});
        record(&keep, 1_800_000_030., &limits);
        assert_eq!(
            std::fs::read_to_string(keep.join(FILE))
                .unwrap()
                .lines()
                .count(),
            1
        );
    }
}
