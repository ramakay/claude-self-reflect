//! A report in words: the lines the card shows.
use super::eval::{CheckResult, Headline, Report, Status, Trend, Val};
use super::spec::{Kind, Op, Trial};
use chrono::TimeZone;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Row {
    /// "✓", "✗" or "…" ("→" for a long road still pending).
    pub mark: String,
    /// "ok", "red" or "quiet": the colour of the mark.
    pub sev: String,
    pub text: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Card {
    pub title: String,
    /// The build on trial: "combo ac9c24c3".
    pub candidate: String,
    /// "ok", "amber", "red" or "quiet": the colour of the first line.
    pub sev: String,
    pub line1: String,
    pub line2: String,
    /// How much of the planned time has passed, 0 to 1.
    pub progress: f64,
    pub rows: Vec<Row>,
    pub warning: Option<String>,
    /// When the words next change with no new data, as a time: the clock line moves by the minute.
    #[serde(default)]
    pub next_change: Option<f64>,
    /// The maintainer's long road, as one line under the card; only when the road is switched on
    /// and a run trial makes the card.
    #[serde(default)]
    pub road: Option<Row>,
}

/// The next moment the clock line's minutes change.
fn next_minute(trial: &Trial, now: f64) -> f64 {
    if now < trial.end {
        // The span left counts down: its minute drops when the seconds past it run out.
        now + ((trial.end - now) % 60.).max(1.)
    } else {
        now + 60. - (now - trial.end) % 60.
    }
}

const TITLE: &str = "CSR performance";

/// 5h 32m, 42m, under 1m. Seconds only when the whole span is under a minute.
fn span(secs: f64) -> String {
    let minutes = (secs.max(0.) / 60.).floor() as i64;
    match minutes {
        0 if (1. ..60.).contains(&secs) => format!("{}s", secs as i64),
        0 => "under 1m".into(),
        m if m >= 60 => format!("{}h {}m", m / 60, m % 60),
        m => format!("{m}m"),
    }
}

/// "Combo soak · ends 12:04 AM · 5h 32m left", with the day when the end is not today.
fn clock<Tz: TimeZone>(trial: &Trial, now: f64, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let when = tz.timestamp_opt(trial.end as i64, 0).single();
    let today = tz.timestamp_opt(now as i64, 0).single();
    let time = match (when, today) {
        (Some(w), Some(t)) => {
            let day = if w.date_naive() == t.date_naive() {
                ""
            } else {
                "%a "
            };
            w.format(&format!("{day}%-I:%M %p")).to_string()
        }
        _ => String::new(),
    };
    if now >= trial.end {
        format!(
            "{} · ended {} · {} ago",
            trial.name,
            time,
            span(now - trial.end)
        )
    } else {
        format!(
            "{} · ends {} · {} left",
            trial.name,
            time,
            span(trial.end - now)
        )
    }
}

fn candidate(trial: &Trial) -> String {
    let sha: String = trial.candidate.sha.chars().take(8).collect();
    format!("{} {}", trial.candidate.label, sha)
        .trim()
        .to_owned()
}

/// A number as a person reads it: whole above a hundred, a tenth below, never "-0".
fn number(n: f64, signed: bool) -> String {
    let mut s = if n.abs() >= 100. {
        format!("{n:.0}")
    } else {
        let s = format!("{n:.1}");
        s.strip_suffix(".0").map(str::to_owned).unwrap_or(s)
    };
    if s == "-0" {
        s = "0".into();
    }
    if signed && !s.starts_with('-') && s != "0" {
        s.insert(0, '+');
    }
    s
}
/// "6 h", "0.7 h": hours as a person says them.
fn hours(h: f64) -> String {
    format!("{} h", number((h * 10.).round() / 10., false))
}
fn value_text(v: Val, signed: bool) -> String {
    match v {
        Val::Num(n) => number(n, signed),
        Val::Bool(b) => if b { "yes" } else { "no" }.into(),
    }
}

fn row(c: &CheckResult) -> Row {
    let s = &c.spec;
    let signed = s.kind == Kind::SlopePerHour;
    let limit = match &s.threshold {
        Value::Bool(b) => if *b { "yes" } else { "no" }.to_owned(),
        other => number(other.as_f64().unwrap_or(0.), signed),
    };
    let unit = if s.unit.is_empty() {
        String::new()
    } else {
        format!(" {}", s.unit)
    };
    let word = if matches!(s.op, Op::Ge | Op::Gt) {
        "need"
    } else {
        "limit"
    };
    let arrow = match c.trend {
        Some(Trend::Up) => " ↑",
        Some(Trend::Down) => " ↓",
        Some(Trend::Flat) => " →",
        None => "",
    };
    let label = &s.label;
    let text = match (s.kind, c.status, c.value) {
        (Kind::AtLeastByEnd, status, v) => {
            let (shown, sep) = match v {
                Some(v @ Val::Bool(_)) => (value_text(v, false), ": "),
                Some(v) => (value_text(v, false), " "),
                None => (
                    if status == Status::Fail {
                        "no data"
                    } else {
                        "no data yet"
                    }
                    .into(),
                    " ",
                ),
            };
            match status {
                Status::Pending => format!("{label}{sep}{shown}, need {limit} by end"),
                Status::Fail => format!("{label}{sep}{shown}, needed {limit}"),
                Status::Pass => format!("{label}{sep}{shown} (need {limit})"),
            }
        }
        (_, status, None) if c.short_span.is_some() => {
            let (have, need) = c.short_span.unwrap_or_default();
            if status == Status::Fail {
                format!(
                    "{label}: only {} of samples, needed {}",
                    hours(have),
                    hours(need)
                )
            } else {
                format!(
                    "{label}: waiting for {} of samples (have {})",
                    hours(need),
                    // Nearest tenth, but never "have 6 h" of a window still short of 6.
                    hours(if (have * 10.).round() / 10. >= need {
                        need - 0.1
                    } else {
                        have
                    })
                )
            }
        }
        (_, Status::Fail, None) => format!("{label} had no data"),
        (_, _, None) => format!("{label} waiting for data"),
        (_, _, Some(v)) => {
            let arrow = if matches!(v, Val::Bool(_)) { "" } else { arrow };
            format!(
                "{label} {}{unit}{arrow} ({word} {limit})",
                value_text(v, signed)
            )
        }
    };
    let (mark, sev) = match c.status {
        Status::Pass => ("✓", "ok"),
        Status::Fail => ("✗", "red"),
        Status::Pending => ("…", "quiet"),
    };
    Row {
        mark: mark.into(),
        sev: sev.into(),
        text,
    }
}

pub fn card<Tz: TimeZone>(report: &Report, tz: &Tz) -> Card
where
    Tz::Offset: std::fmt::Display,
{
    let total = report.checks.len();
    let headline = match &report.headline {
        Headline::OnTrack => "On track".to_owned(),
        Headline::Waiting => "Waiting for data".to_owned(),
        Headline::Failing(labels) if labels.len() > 1 => {
            format!("Failing: {} +{} more", labels[0], labels.len() - 1)
        }
        Headline::Failing(labels) => format!("Failing: {}", labels.join("")),
        Headline::FinishedClean => "Soak finished clean".to_owned(),
        Headline::FinishedFailed(n) => format!("Soak finished: {n} failed"),
    };
    let sev = match report.headline {
        Headline::OnTrack | Headline::FinishedClean => "ok",
        Headline::Waiting => "quiet",
        Headline::Failing(_) | Headline::FinishedFailed(_) => "red",
    };
    let waiting = if report.pending > 0 {
        format!(" · {} waiting", report.pending)
    } else {
        String::new()
    };
    Card {
        title: TITLE.into(),
        candidate: candidate(&report.trial),
        sev: sev.into(),
        line1: format!(
            "{} of {total} checks passing{waiting} · {headline}",
            report.passing
        ),
        line2: clock(&report.trial, report.now, tz),
        progress: report.progress,
        rows: report.checks.iter().map(row).collect(),
        warning: report.stale_for.map(|age| {
            format!(
                "No new data for {}, expected every {}",
                span(age),
                span(report.interval)
            )
        }),
        next_change: Some(next_minute(&report.trial, report.now)),
        road: None,
    }
}

/// The first check not yet passing, which the road names as what is next.
fn next_label(report: &Report) -> Option<&str> {
    report
        .checks
        .iter()
        .find(|c| c.status != Status::Pass)
        .map(|c| c.spec.label.as_str())
}

/// The road as one line: "Road to blind 4.5 stars · 3 of 7 · next: Recap judge: recaps worse".
pub fn road_line(report: &Report) -> Row {
    let total = report.checks.len();
    let next = match next_label(report) {
        Some(label) => format!("next: {label}"),
        None => "all passing".to_owned(),
    };
    let (mark, sev) = if report.checks.iter().any(|c| c.status == Status::Fail) {
        ("✗", "red")
    } else if report.pending == 0 {
        ("✓", "ok")
    } else {
        ("→", "quiet")
    };
    Row {
        mark: mark.into(),
        sev: sev.into(),
        text: format!("Long road · {} of {total} · {next}", report.passing),
    }
}

/// The card for a road with no run trial beside it: the usual card, with "next: <check>" in
/// place of the soak's headline, which says "Soak finished" after the end.
pub fn road_card<Tz: TimeZone>(report: &Report, tz: &Tz) -> Card
where
    Tz::Offset: std::fmt::Display,
{
    let mut card = card(report, tz);
    let next = match next_label(report) {
        Some(label) => format!("next: {label}"),
        None => "all passing".to_owned(),
    };
    card.line1 = format!(
        "{} of {} checks passing · {next}",
        report.passing,
        report.checks.len()
    );
    card
}

/// The card for a trial whose data could not be read: why, and the clock if the spec was.
pub fn unreadable<Tz: TimeZone>(trial: Option<&Trial>, reason: &str, now: f64, tz: &Tz) -> Card
where
    Tz::Offset: std::fmt::Display,
{
    Card {
        title: TITLE.into(),
        candidate: trial.map(candidate).unwrap_or_default(),
        sev: "amber".into(),
        line1: format!("Trial data unreadable: {reason}"),
        line2: trial.map(|t| clock(t, now, tz)).unwrap_or_default(),
        progress: trial
            .map(|t| ((now - t.start) / (t.end - t.start)).clamp(0., 1.))
            .unwrap_or(0.),
        rows: vec![],
        warning: None,
        next_change: trial.map(|t| next_minute(t, now)),
        road: None,
    }
}
