//! Evaluate a trial's checks against its samples at a moment.
use super::samples::{lookup, Sample};
use super::spec::{CheckSpec, Kind, Op, Trial};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Pass,
    Fail,
    Pending,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Val {
    Num(f64),
    Bool(bool),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trend {
    Up,
    Down,
    Flat,
}
#[derive(Clone, Debug)]
pub struct CheckResult {
    pub spec: CheckSpec,
    pub status: Status,
    /// What the check measured: the last value, the worst, or the slope. `None` before any data.
    pub value: Option<Val>,
    /// Where the metric itself went over the last three samples.
    pub trend: Option<Trend>,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Headline {
    OnTrack,
    Waiting,
    /// The labels of the failing checks.
    Failing(Vec<String>),
    FinishedClean,
    FinishedFailed(usize),
}
#[derive(Clone, Debug)]
pub struct Report {
    pub trial: Trial,
    pub now: f64,
    pub checks: Vec<CheckResult>,
    pub passing: usize,
    pub failing: usize,
    pub pending: usize,
    pub samples: usize,
    pub newest: Option<f64>,
    /// Seconds between samples.
    pub interval: f64,
    /// How long the newest sample is overdue, past twice the interval, while the trial runs.
    pub stale_for: Option<f64>,
    pub finished: bool,
    /// How much of the planned time has passed, 0 to 1.
    pub progress: f64,
    pub headline: Headline,
}

/// Does `value` hold against `threshold`? `None` when the two are not the same kind of thing.
fn holds(value: Val, op: Op, threshold: &Value) -> Option<bool> {
    match (value, threshold) {
        (Val::Num(v), Value::Number(t)) => {
            let t = t.as_f64()?;
            Some(match op {
                Op::Le => v <= t,
                Op::Lt => v < t,
                Op::Ge => v >= t,
                Op::Gt => v > t,
                Op::Eq => v == t,
                Op::Ne => v != t,
            })
        }
        (Val::Bool(v), Value::Bool(t)) => match op {
            Op::Eq => Some(v == *t),
            Op::Ne => Some(v != *t),
            _ => None,
        },
        _ => None,
    }
}

fn val(row: &Value, metric: &str) -> Option<Val> {
    match lookup(row, metric)? {
        Value::Number(n) => n.as_f64().map(Val::Num),
        Value::Bool(b) => Some(Val::Bool(*b)),
        _ => None,
    }
}

/// The least-squares slope of the points (hours, value), when there are three and they differ in time.
fn slope(points: &[(f64, f64)]) -> Option<f64> {
    if points.len() < 3 {
        return None;
    }
    let n = points.len() as f64;
    let mx = points.iter().map(|p| p.0).sum::<f64>() / n;
    let my = points.iter().map(|p| p.1).sum::<f64>() / n;
    let var: f64 = points.iter().map(|p| (p.0 - mx).powi(2)).sum();
    (var > 0.).then(|| points.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum::<f64>() / var)
}

fn trend(samples: &[&Sample], metric: &str) -> Option<Trend> {
    let last: Vec<f64> = samples
        .iter()
        .rev()
        .take(3)
        .rev()
        .filter_map(|s| match val(&s.row, metric) {
            Some(Val::Num(n)) => Some(n),
            _ => None,
        })
        .collect();
    let (first, end) = (*last.first()?, *last.last()?);
    if last.len() < 2 {
        return None;
    }
    let scale = first.abs().max(end.abs());
    Some(if (end - first).abs() <= scale * 0.01 {
        Trend::Flat
    } else if end > first {
        Trend::Up
    } else {
        Trend::Down
    })
}

fn check(spec: &CheckSpec, trial: &Trial, samples: &[&Sample], now: f64) -> CheckResult {
    let numbers = |from: f64| -> Vec<(f64, f64)> {
        samples
            .iter()
            .filter(|s| s.at >= from)
            .filter_map(|s| match val(&s.row, &spec.metric) {
                Some(Val::Num(n)) => Some((s.at, n)),
                _ => None,
            })
            .collect()
    };
    let latest = samples.iter().rev().find_map(|s| val(&s.row, &spec.metric));
    let (mut status, value) = match spec.kind {
        Kind::Latest => (None, latest),
        Kind::Max => {
            let all = numbers(f64::MIN);
            let worst = match spec.op {
                Op::Le | Op::Lt => all.iter().map(|p| p.1).reduce(f64::max),
                _ => all.iter().map(|p| p.1).reduce(f64::min),
            };
            (None, worst.map(Val::Num))
        }
        Kind::SlopePerHour => {
            let from = trial.start + spec.warmup_hours * 3600.;
            let points: Vec<(f64, f64)> = numbers(from)
                .into_iter()
                .map(|(at, v)| ((at - from) / 3600., v))
                .collect();
            (None, slope(&points).map(Val::Num))
        }
        Kind::AtLeastByEnd => {
            let met = samples.iter().find_map(|s| {
                let v = val(&s.row, &spec.metric)?;
                holds(v, spec.op, &spec.threshold)
                    .filter(|ok| *ok)
                    .map(|_| v)
            });
            match met {
                Some(v) => (Some(Status::Pass), Some(v)),
                None => (Some(Status::Pending), latest),
            }
        }
    };
    if status.is_none() {
        status = Some(
            match value.and_then(|v| holds(v, spec.op, &spec.threshold)) {
                Some(true) => Status::Pass,
                Some(false) => Status::Fail,
                None => Status::Pending,
            },
        );
    }
    let mut status = status.unwrap_or(Status::Pending);
    // At the planned end nothing is pending any more: what was not shown to hold did not.
    if status == Status::Pending && now >= trial.end {
        status = Status::Fail;
    }
    CheckResult {
        spec: spec.clone(),
        status,
        value,
        trend: trend(samples, &spec.metric),
    }
}

/// Seconds between samples: the spec's word, else the median gap of the last ten, else ten minutes.
fn interval(trial: &Trial, samples: &[&Sample]) -> f64 {
    if let Some(n) = trial.sample_interval_secs.filter(|n| *n > 0.) {
        return n;
    }
    let tail = &samples[samples.len().saturating_sub(10)..];
    let mut gaps: Vec<f64> = tail
        .windows(2)
        .map(|w| w[1].at - w[0].at)
        .filter(|g| *g > 0.)
        .collect();
    gaps.sort_by(f64::total_cmp);
    gaps.get(gaps.len() / 2).copied().unwrap_or(600.)
}

pub fn evaluate(trial: &Trial, samples: &[Sample], now: f64) -> Report {
    // The run's own samples: from its start to its planned end.
    let mine: Vec<&Sample> = samples
        .iter()
        .filter(|s| s.at >= trial.start && s.at <= trial.end)
        .collect();
    let checks: Vec<CheckResult> = trial
        .checks
        .iter()
        .map(|c| check(c, trial, &mine, now))
        .collect();
    let count = |s: Status| checks.iter().filter(|c| c.status == s).count();
    let (passing, failing, pending) = (
        count(Status::Pass),
        count(Status::Fail),
        count(Status::Pending),
    );
    let finished = now >= trial.end;
    let newest = mine.last().map(|s| s.at);
    let interval = interval(trial, &mine);
    let stale_for = newest
        .map(|at| now - at)
        .filter(|age| !finished && *age > 2. * interval);
    let headline = if finished {
        if failing == 0 {
            Headline::FinishedClean
        } else {
            Headline::FinishedFailed(failing)
        }
    } else if failing > 0 {
        Headline::Failing(
            checks
                .iter()
                .filter(|c| c.status == Status::Fail)
                .map(|c| c.spec.label.clone())
                .collect(),
        )
    } else if mine.is_empty() || checks.iter().all(|c| c.value.is_none()) {
        Headline::Waiting
    } else {
        Headline::OnTrack
    };
    Report {
        trial: trial.clone(),
        now,
        checks,
        passing,
        failing,
        pending,
        samples: mine.len(),
        newest,
        interval,
        stale_for,
        finished,
        progress: ((now - trial.start) / (trial.end - trial.start)).clamp(0., 1.),
        headline,
    }
}
