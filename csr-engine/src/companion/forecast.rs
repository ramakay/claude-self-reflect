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

/// Where a window is headed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Forecast {
    /// The percentage the window reaches at its reset at the recent pace, capped at 100.
    pub at_reset: f64,
    /// When the window fills before its reset, when it does.
    pub runs_out: Option<f64>,
}

/// The forecast of one window from its readings, or `None` when they cannot back one: fewer than
/// `MIN_SAMPLES`, spanning less than `MIN_SPAN` of the window, or last taken too long ago to say
/// where the figure is now. `samples` are the readings of this window, oldest first.
pub fn project(len: f64, used: f64, resets: f64, now: f64, samples: &[Sample]) -> Option<Forecast> {
    let left = resets - now;
    if !(left > 0. && left <= len && used.is_finite() && (0. ..=100.).contains(&used)) {
        return None;
    }
    let opened = resets - len;
    let seen: Vec<_> = samples
        .iter()
        .filter(|s| s.at >= opened && s.at <= now + 60.)
        .collect();
    if seen.len() < MIN_SAMPLES {
        return None;
    }
    let (first, last) = (seen[0], seen[seen.len() - 1]);
    if last.at - first.at < len * MIN_SPAN || now - last.at > (len * 0.05).max(900.) {
        return None;
    }
    // The recent pace: from the oldest reading in the newest half of what was seen (at most half
    // a window back), so a quiet night does not hold down a busy afternoon for the whole week.
    let back = ((last.at - first.at) / 2.)
        .min(len / 2.)
        .max(len * MIN_SPAN / 2.);
    let anchor = seen
        .iter()
        .find(|s| last.at - s.at <= back)
        .copied()
        .unwrap_or(first);
    let anchor = if last.at - anchor.at >= len * MIN_SPAN / 2. {
        anchor
    } else {
        first
    };
    let rate = ((used - anchor.used) / (last.at - anchor.at)).max(0.);
    let reach = used + rate * left;
    let runs_out = (rate > 0. && used < 100.)
        .then(|| now + (100. - used) / rate)
        .filter(|at| *at < resets);
    Some(Forecast {
        at_reset: reach.min(100.),
        runs_out,
    })
}

/// The words under a circle: where the window ends up, or roughly when it fills.
pub fn caption(f: &Forecast, now: f64) -> (String, bool) {
    match f.runs_out {
        Some(at) => (format!("out ~{}", masthead::reset_label(at, now)), true),
        None => (format!("→ ~{:.0}% at reset", f.at_reset.round()), false),
    }
}

/// The forecast of each window of `rate_limits` (the masthead's shape), as `{"5H": {"text",
/// "out"}}` for a figure's quotas. A window with no forecast has no key.
pub fn for_limits(keep: &Path, rate_limits: &Value, now: f64) -> Value {
    let mut out = serde_json::Map::new();
    let rows = history(keep, now);
    for (code, kind, len) in WINDOWS {
        let window = &rate_limits[kind];
        let (Some(used), Some(resets)) = (
            window["used_percentage"].as_f64(),
            window["resets_at"].as_f64(),
        ) else {
            continue;
        };
        let samples: Vec<_> = rows
            .iter()
            .filter(|(k, s)| k == kind && (s.resets - resets).abs() < SAME_WINDOW)
            .map(|(_, s)| *s)
            .collect();
        let Some(forecast) = project(len, used, resets, now, &samples) else {
            continue;
        };
        let (text, out_of) = caption(&forecast, now);
        out.insert(
            code.into(),
            json!({"text": text::one_line(&text, 40), "out": out_of}),
        );
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEN: f64 = 18000.;

    /// `n` readings from the window's opening, one every `step` seconds, rising `per` points each.
    fn climb(resets: f64, n: usize, step: f64, per: f64) -> Vec<Sample> {
        (0..n)
            .map(|i| Sample {
                at: resets - LEN + 60. + i as f64 * step,
                used: 5. + per * i as f64,
                resets,
            })
            .collect()
    }

    #[test]
    fn test_too_few_or_too_short_readings_say_nothing() {
        let resets = 1_800_018_000.;
        let rows = climb(resets, 12, 300., 1.);
        let at = rows[rows.len() - 1].at + 30.;
        let used = rows[rows.len() - 1].used;
        assert!(project(LEN, used, resets, at, &rows).is_some());
        // Five readings are not enough.
        assert_eq!(project(LEN, used, resets, at, &rows[7..]), None);
        // Many readings squeezed into a few minutes span too little of the window.
        let tight = climb(resets, 20, 10., 0.1);
        let t = tight[19].at + 5.;
        assert_eq!(project(LEN, tight[19].used, resets, t, &tight), None);
        // Readings that stopped long ago cannot say where the figure is now.
        assert_eq!(project(LEN, used, resets, at + 3000., &rows), None);
        // Nothing before the window opened counts.
        let old: Vec<_> = rows
            .iter()
            .map(|s| Sample {
                at: s.at - 2. * LEN,
                resets: resets - 2. * LEN,
                ..*s
            })
            .collect();
        assert_eq!(project(LEN, used, resets, at, &old), None);
    }

    #[test]
    fn test_a_steady_climb_is_carried_to_the_reset() {
        let resets = 1_800_018_000.;
        // 12 readings 5 minutes apart, +1 point each: 0.2 points a minute, 11 points seen.
        let rows = climb(resets, 12, 300., 1.);
        let last = rows[11];
        let now = last.at + 60.;
        let f = project(LEN, last.used, resets, now, &rows).unwrap();
        let left = resets - now;
        let want = last.used + left * (1. / 300.);
        assert!((f.at_reset - want.min(100.)).abs() < 0.5, "{f:?} vs {want}");
        assert!(f.at_reset > last.used);
    }

    #[test]
    fn test_a_pace_that_fills_the_window_early_names_roughly_when() {
        let resets = 1_800_018_000.;
        // 4 points every 5 minutes: 0.8 points a minute.
        let rows = climb(resets, 12, 300., 4.);
        let last = rows[11];
        let now = last.at;
        let f = project(LEN, last.used, resets, now, &rows).unwrap();
        assert_eq!(f.at_reset, 100.);
        let out = f.runs_out.expect("fills before the reset");
        let want = now + (100. - last.used) / (4. / 300.);
        assert!((out - want).abs() < 5., "{out} vs {want}");
        assert!(out > now && out < resets);
        let (text, hot) = caption(&f, now);
        assert!(hot && text.starts_with("out ~"), "{text}");
    }

    #[test]
    fn test_a_flat_window_projects_where_it_stands() {
        let resets = 1_800_018_000.;
        let rows = climb(resets, 12, 300., 0.);
        let last = rows[11];
        let f = project(LEN, last.used, resets, last.at + 10., &rows).unwrap();
        assert_eq!(f.at_reset, 5.);
        assert_eq!(f.runs_out, None);
        assert_eq!(caption(&f, last.at).0, "→ ~5% at reset");
    }

    #[test]
    fn test_the_recent_pace_counts_over_a_quiet_start() {
        let resets = 1_800_018_000.;
        // An hour of nothing, then a fast hour.
        let mut rows = climb(resets, 12, 300., 0.);
        let base = rows[11];
        for i in 1..=12 {
            rows.push(Sample {
                at: base.at + 300. * i as f64,
                used: 5. + 2. * i as f64,
                resets,
            });
        }
        let last = rows[rows.len() - 1];
        let f = project(LEN, last.used, resets, last.at, &rows).unwrap();
        // The whole-window average would be 29 points over 2h, much less than the recent pace.
        let averaged = last.used + (resets - last.at) * (last.used - 5.) / (last.at - rows[0].at);
        assert!(f.at_reset > averaged.min(100.), "{f:?} vs {averaged}");
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

    #[test]
    fn test_for_limits_abstains_without_history_and_forecasts_with_it() {
        let d = tempfile::tempdir().unwrap();
        let keep = d.path().join("csr-companion");
        let resets = 1_800_018_000.;
        let rows = climb(resets, 12, 300., 1.);
        let last = rows[11];
        let now = last.at + 30.;
        let limits = json!({"five_hour": {"used_percentage": last.used, "resets_at": resets},
                            "seven_day": {"used_percentage": 30., "resets_at": 1_800_400_000.}});
        // No file: nothing, for either window.
        assert_eq!(for_limits(&keep, &limits, now), json!({}));
        std::fs::create_dir_all(&keep).unwrap();
        let body: String = rows
            .iter()
            .map(|s| format!("{}\n", line_of("five_hour", s)))
            .collect();
        std::fs::write(keep.join(FILE), body).unwrap();
        let got = for_limits(&keep, &limits, now);
        let text = got["5H"]["text"].as_str().expect("a 5H forecast");
        assert!(
            text.starts_with("→ ~") && text.ends_with("% at reset"),
            "{text}"
        );
        // The week has no readings of its own: no number is made up for it.
        assert!(got.get("7D").is_none(), "{got}");
    }
}
