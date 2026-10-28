//! The API credit left on the account, as the api-lanes ledger counts it. The sums are the ones
//! `lanes.py` makes (`credit_left`, `summary`): the credit in `credit.json`, less every ledger
//! row since its date; the pace is the last seven days of rows over seven. `status.json` is not
//! read: the runner rewrites it only on events, so it goes stale.
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Under this many days of credit the usage headline says so.
pub const WARN_DAYS: f64 = 3.;

/// What the pane knows of the Anthropic admin key, for the credit row.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum KeyNote {
    /// Not looked at (admin cost reporting is off): the row says "by ledger".
    #[default]
    Unchecked,
    Connected,
    Missing,
    /// Rejected; the key's type tag.
    Rejected(String),
    Individual,
    Offline,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Credit {
    /// `credit.json` less the ledger since its date. A ledger estimate, not an account balance.
    pub left: f64,
    /// Workers running now, and the most they may still spend (the sum of their caps).
    pub active: usize,
    pub running_caps: f64,
    /// The ledger rows of the last seven days.
    pub week: f64,
    #[serde(default)]
    pub key: KeyNote,
    /// Two keychain entries that differ.
    #[serde(default)]
    pub dup: bool,
    /// Seconds since the real-spend figures were fetched, when the last fetch failed.
    #[serde(default)]
    pub stale: Option<f64>,
    /// Ledger spend counted for UTC days the cost API has not returned yet: (label, dollars), "today" or "on MM-DD".
    #[serde(default)]
    pub ledger_added: Vec<(String, f64)>,
}

/// The folder the runner keeps its files in: `$API_LANES_HOME`, else `~/.agents/api-lanes`.
pub fn dir(home: &Path, env: Option<std::ffi::OsString>) -> PathBuf {
    match env.filter(|e| !e.is_empty()) {
        Some(e) => e.into(),
        None => home.join(".agents/api-lanes"),
    }
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// `credit.json`'s `since` as a time: a date or naive date-time is read in `tz`, as Python does.
fn since<Tz: TimeZone>(text: &str, tz: &Tz) -> Option<f64> {
    if let Ok(t) = DateTime::parse_from_rfc3339(text) {
        return Some(t.timestamp() as f64);
    }
    let naive = NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f")
        .or_else(|_| NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S%.f"))
        .ok()
        .or_else(|| {
            NaiveDate::parse_from_str(text, "%Y-%m-%d")
                .ok()
                .and_then(|d| d.and_hms_opt(0, 0, 0))
        })?;
    tz.from_local_datetime(&naive)
        .earliest()
        .map(|t| t.timestamp() as f64)
}

fn alive(pid: i64) -> bool {
    if pid <= 0 || pid > i32::MAX as i64 {
        return false;
    }
    // SAFETY: signal 0 only asks whether the process exists.
    let r = unsafe { libc::kill(pid as i32, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// The credit at `now`, or `None` when there is no credit on record (no `credit.json`, or no
/// amount in it): then the pane says nothing about it.
pub fn read_in<Tz: TimeZone>(dir: &Path, now: f64, tz: &Tz) -> Option<Credit> {
    let credit = read_json(&dir.join("credit.json"))?;
    let amount = credit["amount"].as_f64().filter(|a| *a != 0.)?;
    let from = credit["since"]
        .as_str()
        .filter(|s| !s.is_empty())
        .and_then(|s| since(s, tz))
        .unwrap_or(0.);
    let (mut spent, mut week) = (0., 0.);
    for line in std::fs::read_to_string(dir.join("ledger.jsonl"))
        .unwrap_or_default()
        .lines()
    {
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let at = row["ts"].as_f64().unwrap_or(0.);
        let cost = row["cost"].as_f64().unwrap_or(0.);
        if at >= from {
            spent += cost;
        }
        if at >= now - 7. * 86400. {
            week += cost;
        }
    }
    let (mut active, mut running_caps) = (0, 0.);
    for entry in std::fs::read_dir(dir.join("live"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let Some(run) = read_json(&path) else {
            continue;
        };
        // The runner died: its live file is stale.
        if !alive(run["pid"].as_i64().unwrap_or(0)) {
            continue;
        }
        for worker in run["workers"]
            .as_object()
            .into_iter()
            .flat_map(|w| w.values())
        {
            if worker["state"] == "running" {
                active += 1;
                running_caps += worker["cap"].as_f64().unwrap_or(0.);
            }
        }
    }
    Some(Credit {
        left: amount - spent,
        active,
        running_caps,
        week,
        ..Default::default()
    })
}

pub fn read(dir: &Path, now: f64) -> Option<Credit> {
    read_in(dir, now, &Local)
}

/// [`read`], with real Anthropic spend from the admin cost cache in place of the ledger's when
/// the key allows it, and the key's state either way. A stale cache starts a fetch on a thread;
/// this call only ever reads the cache.
pub fn read_live(dir: &Path, home: &Path, now: f64) -> Option<Credit> {
    use super::admin_cost as ac;
    let mut credit = read(dir, now)?;
    if !ac::enabled() {
        return Some(credit);
    }
    let file = read_json(&dir.join("credit.json"))?;
    let amount = file["amount"].as_f64()?;
    let since = file["since"]
        .as_str()
        .and_then(ac::since_date)
        .unwrap_or_else(|| {
            DateTime::from_timestamp(now as i64, 0)
                .map_or_else(Default::default, |t| t.date_naive())
                - chrono::Duration::days(30)
        });
    let from = ac::wanted_from(since, now);
    let cache = ac::load(&ac::cache_path(home));
    if !cache.as_ref().is_some_and(|c| ac::fresh(c, &from, now)) {
        ac::spawn_refresh(home.to_owned(), from, now);
    }
    let ledger =
        ac::ledger_by_day(&std::fs::read_to_string(dir.join("ledger.jsonl")).unwrap_or_default());
    ac::apply(&mut credit, cache.as_ref(), amount, since, now, &ledger);
    Some(credit)
}

impl Credit {
    /// Dollars a day, at the last seven days' pace.
    pub fn per_day(&self) -> f64 {
        self.week / 7.
    }
    /// The credit the days are counted from: what is left, less every running worker's cap.
    pub fn floor(&self) -> f64 {
        self.left - self.running_caps
    }
    /// Days of credit at the week's pace; `None` when nothing was spent in seven days.
    pub fn days(&self) -> Option<f64> {
        let basis = if self.active > 0 {
            self.floor()
        } else {
            self.left
        };
        (self.per_day() > 0.).then(|| (basis / self.per_day()).max(0.))
    }
    /// The row under the limit bars: what is left, where the figure comes from, and (when the
    /// Anthropic admin key is not giving real spend) what to tell Claude.
    pub fn row(&self) -> String {
        let left = self.left.max(0.);
        let mut out = match &self.key {
            KeyNote::Unchecked => format!("API credit ${left:.2} left by ledger"),
            KeyNote::Connected => {
                let mut t =
                    format!("API credit ${left:.2} left \u{b7} real Anthropic spend (admin key)");
                for (label, cost) in &self.ledger_added {
                    t.push_str(&format!(" + ${cost:.2} {label}"));
                }
                if !self.ledger_added.is_empty() {
                    t.push_str(" by ledger");
                }
                t
            }
            _ => format!("API credit ${left:.2} left (ledger estimate)"),
        };
        match &self.key {
            KeyNote::Unchecked | KeyNote::Connected => match self.days() {
                Some(days) => out.push_str(&format!(
                    " \u{b7} {} at this week's pace (${}/day)",
                    span(days),
                    money_a_day(self.per_day())
                )),
                None => out.push_str(" \u{b7} no spend in 7 days"),
            },
            KeyNote::Missing => out.push_str(
                " \u{b7} for real spend, tell Claude: connect my Anthropic admin key",
            ),
            KeyNote::Rejected(tag) => out.push_str(&format!(
                " \u{b7} Admin key rejected ({tag} key) \u{b7} needs an admin or org-scoped key \u{b7} tell Claude: fix my admin key"
            )),
            KeyNote::Individual => {
                out.push_str(" \u{b7} Admin API not available on this account \u{b7} using the ledger")
            }
            KeyNote::Offline => out.push_str(" \u{b7} Anthropic cost API unreachable"),
        }
        if self.active > 0 {
            out.push_str(&format!(
                " \u{b7} up to ${:.2} still running",
                self.running_caps
            ));
        }
        if let Some(age) = self.stale {
            out.push_str(&format!(" \u{b7} spend as of {}", ago(age)));
        }
        if self.dup {
            out.push_str(" \u{b7} 2 keychain entries; using the admin one");
        }
        out
    }
    /// The usage headline when the credit runs out soon: its words and its colour. Amber from
    /// under three days, red from under one.
    pub fn headline(&self) -> Option<(String, &'static str)> {
        let days = self.days().filter(|d| *d < WARN_DAYS)?;
        let text = if days <= 0. {
            "API credit is used up".to_owned()
        } else {
            format!("API credit runs out in {}", span(days))
        };
        Some((text, if days < 1. { "red" } else { "amber" }))
    }
}

/// "just now", "12 min ago", "2 h ago", "3 days ago".
fn ago(secs: f64) -> String {
    match secs {
        s if s < 60. => "just now".into(),
        s if s < 3600. => format!("{} min ago", (s / 60.).round() as i64),
        s if s < 86400. => format!("{} h ago", (s / 3600.).round() as i64),
        s => format!("{} days ago", (s / 86400.).round() as i64),
    }
}

/// "about 2 days", "about 1 day", "less than a day".
fn span(days: f64) -> String {
    if days < 1. {
        return "less than a day".into();
    }
    let n = days.round() as i64;
    format!("about {n} day{}", if n == 1 { "" } else { "s" })
}
fn money_a_day(x: f64) -> String {
    if x >= 10. {
        format!("{x:.1}")
    } else {
        format!("{x:.2}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::fs;

    const NOW: f64 = 1792320000.;

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/companion/api-lanes")
    }
    /// A copy of the fixture to add live files to.
    fn copy() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        for f in ["credit.json", "ledger.jsonl"] {
            fs::copy(fixture().join(f), d.path().join(f)).unwrap();
        }
        d
    }

    #[test]
    fn the_sums_are_the_ones_lanes_py_makes_on_the_same_files() {
        // Pinned from `lanes.summary()` with `now` fixed at 1792320000 and TZ=UTC on this
        // fixture: credit_left 188.25, week 21.75, active 0.
        let c = read_in(&fixture(), NOW, &Utc).unwrap();
        assert_eq!(c.left, 188.25);
        assert_eq!(c.week, 21.75);
        assert_eq!((c.active, c.running_caps), (0, 0.));
    }

    #[test]
    fn and_so_are_they_when_lanes_py_itself_is_here() {
        let lanes = PathBuf::from(std::env::var("HOME").unwrap_or_default())
            .join(".claude/skills/api-lanes/scripts/lanes.py");
        let Ok(out) = std::process::Command::new("python3")
            .arg("-I")
            .arg("-c")
            .arg("import sys,json;sys.path.insert(0,sys.argv[1]);import lanes;lanes.now=lambda:1792320000;s=lanes.summary();print(json.dumps([s['credit_left'],s['week'],s['active']]))")
            .arg(lanes.parent().unwrap_or(Path::new("/")))
            .env("TZ", "UTC")
            .env("API_LANES_HOME", fixture())
            .output()
        else {
            return;
        };
        if !lanes.exists() || !out.status.success() {
            return;
        }
        let want: Vec<f64> = serde_json::from_slice(&out.stdout).unwrap();
        let c = read_in(&fixture(), NOW, &Utc).unwrap();
        assert_eq!(want, [c.left, c.week, c.active as f64]);
    }

    #[test]
    fn the_row_names_what_is_left_the_pace_and_the_days() {
        let c = Credit {
            left: 29.23,
            active: 0,
            running_caps: 0.,
            week: 94.63,
            ..Default::default()
        };
        assert_eq!(
            c.row(),
            "API credit $29.23 left by ledger · about 2 days at this week's pace ($13.5/day)"
        );
        let c = read_in(&fixture(), NOW, &Utc).unwrap();
        assert_eq!(
            c.row(),
            "API credit $188.25 left by ledger · about 61 days at this week's pace ($3.11/day)"
        );
    }

    #[test]
    fn running_workers_add_their_caps_and_the_days_count_from_the_floor() {
        let c = Credit {
            left: 29.23,
            active: 2,
            running_caps: 12.,
            week: 94.63,
            ..Default::default()
        };
        assert_eq!(c.floor(), 17.23);
        assert_eq!(
            c.row(),
            "API credit $29.23 left by ledger · about 1 day at this week's pace ($13.5/day) · up to $12.00 still running"
        );
    }

    #[test]
    fn live_files_of_a_living_runner_count_and_a_dead_runners_do_not() {
        let d = copy();
        fs::create_dir(d.path().join("live")).unwrap();
        let me = std::process::id();
        fs::write(
            d.path().join("live/mine.json"),
            serde_json::json!({"pid":me,"run":"r","workers":{
                "a":{"state":"running","cap":5.0},"b":{"state":"done","cap":9.0},"c":{"state":"running","cap":2.5}}})
            .to_string(),
        )
        .unwrap();
        fs::write(
            d.path().join("live/dead.json"),
            serde_json::json!({"pid":999999,"run":"r2","workers":{"z":{"state":"running","cap":50.0}}}).to_string(),
        )
        .unwrap();
        fs::write(d.path().join("live/junk.json"), "{").unwrap();
        let c = read_in(d.path(), NOW, &Utc).unwrap();
        assert_eq!((c.active, c.running_caps), (2, 7.5));
        assert_eq!(c.floor(), 188.25 - 7.5);
    }

    #[test]
    fn under_three_days_the_headline_says_so_in_amber_and_under_one_in_red() {
        let two = Credit {
            left: 29.23,
            active: 0,
            running_caps: 0.,
            week: 94.63,
            ..Default::default()
        };
        assert_eq!(
            two.headline(),
            Some(("API credit runs out in about 2 days".into(), "amber"))
        );
        let less = Credit {
            left: 5.,
            active: 0,
            running_caps: 0.,
            week: 94.63,
            ..Default::default()
        };
        assert_eq!(
            less.headline(),
            Some(("API credit runs out in less than a day".into(), "red"))
        );
        let plenty = Credit {
            left: 188.25,
            active: 0,
            running_caps: 0.,
            week: 21.75,
            ..Default::default()
        };
        assert_eq!(plenty.headline(), None);
        let spent = Credit {
            left: -1.,
            active: 0,
            running_caps: 0.,
            week: 10.,
            ..Default::default()
        };
        assert_eq!(
            spent.headline(),
            Some(("API credit is used up".into(), "red"))
        );
    }

    #[test]
    fn no_spend_in_seven_days_shows_no_days_figure_and_no_warning() {
        let c = Credit {
            left: 29.23,
            active: 0,
            running_caps: 0.,
            week: 0.,
            ..Default::default()
        };
        assert_eq!(c.days(), None);
        assert_eq!(
            c.row(),
            "API credit $29.23 left by ledger · no spend in 7 days"
        );
        assert_eq!(c.headline(), None);
        // The fixture's ledger is old news a week on.
        let later = read_in(&fixture(), NOW + 8. * 86400., &Utc).unwrap();
        assert_eq!(later.week, 0.);
        assert!(later.row().ends_with("· no spend in 7 days"));
    }

    #[test]
    fn a_missing_or_broken_credit_file_hides_the_row() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(read_in(d.path(), NOW, &Utc), None);
        fs::write(d.path().join("credit.json"), "{").unwrap();
        assert_eq!(read_in(d.path(), NOW, &Utc), None);
        fs::write(d.path().join("credit.json"), "{\"amount\": 0}").unwrap();
        assert_eq!(read_in(d.path(), NOW, &Utc), None);
        assert_eq!(read_in(&d.path().join("nope"), NOW, &Utc), None);
    }

    #[test]
    fn the_base_is_credit_json_so_a_top_up_changes_only_the_amount() {
        let d = copy();
        fs::write(
            d.path().join("credit.json"),
            "{\"amount\": 300.0, \"since\": \"2026-10-16\"}",
        )
        .unwrap();
        assert_eq!(read_in(d.path(), NOW, &Utc).unwrap().left, 288.25);
        // A new base date drops the rows before it.
        fs::write(
            d.path().join("credit.json"),
            "{\"amount\": 300.0, \"since\": \"2026-10-18\"}",
        )
        .unwrap();
        assert_eq!(read_in(d.path(), NOW, &Utc).unwrap().left, 300. - 1.25);
    }

    #[test]
    fn the_folder_is_the_env_var_else_the_home_default() {
        let home = Path::new("/h");
        assert_eq!(dir(home, None), PathBuf::from("/h/.agents/api-lanes"));
        assert_eq!(
            dir(home, Some("".into())),
            PathBuf::from("/h/.agents/api-lanes")
        );
        assert_eq!(dir(home, Some("/x".into())), PathBuf::from("/x"));
    }
}
