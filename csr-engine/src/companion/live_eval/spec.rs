//! The trial spec: what to watch, where the samples are, and what each check must hold to.
use chrono::DateTime;
use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Candidate {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub sha: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub enum Op {
    #[serde(rename = "<=")]
    Le,
    #[serde(rename = "<")]
    Lt,
    #[serde(rename = ">=")]
    Ge,
    #[serde(rename = ">")]
    Gt,
    #[serde(rename = "==")]
    Eq,
    #[serde(rename = "!=")]
    Ne,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// The last sample against the threshold.
    Latest,
    /// The worst sample of the run: the highest against a `<` limit, the lowest against a `>` one.
    Max,
    /// The least-squares slope per hour over the samples after a warm-up.
    SlopePerHour,
    /// Pending until some sample meets it; fails only at the planned end.
    AtLeastByEnd,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CheckSpec {
    pub id: String,
    pub label: String,
    /// The field of a sample: dots step into objects, a number steps into a list.
    pub metric: String,
    pub op: Op,
    pub threshold: Value,
    pub kind: Kind,
    /// For `slope_per_hour`: hours from the start whose samples are left out.
    #[serde(default)]
    pub warmup_hours: f64,
    /// Said after a value: "ms", "MB/h".
    #[serde(default)]
    pub unit: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Trial {
    pub name: String,
    pub started_at: String,
    pub planned_end: String,
    #[serde(default)]
    pub candidate: Candidate,
    /// The NDJSON samples file. A relative path is read from the folder of the spec.
    pub source: String,
    pub checks: Vec<CheckSpec>,
    /// Seconds between samples, when the samples do not say so themselves.
    #[serde(default)]
    pub sample_interval_secs: Option<f64>,
    /// The field of a sample that holds its time (RFC 3339).
    #[serde(default = "time_field")]
    pub time_field: String,
    #[serde(skip)]
    pub start: f64,
    #[serde(skip)]
    pub end: f64,
}
fn time_field() -> String {
    "t".into()
}

fn epoch(text: &str, what: &str) -> Result<f64, String> {
    DateTime::parse_from_rfc3339(text)
        .map(|t| t.timestamp() as f64)
        .map_err(|_| format!("{what} {text:?} is not an RFC 3339 time"))
}

impl Trial {
    pub fn from_json(text: &str) -> Result<Trial, String> {
        let mut trial: Trial = serde_json::from_str(text).map_err(|e| e.to_string())?;
        trial.start = epoch(&trial.started_at, "started_at")?;
        trial.end = epoch(&trial.planned_end, "planned_end")?;
        if trial.end <= trial.start {
            return Err("planned_end is not after started_at".into());
        }
        for check in &trial.checks {
            let number = check.threshold.is_number();
            if !number && !check.threshold.is_boolean() {
                return Err(format!(
                    "check {}: the threshold is a number or true/false",
                    check.id
                ));
            }
            if !number && !matches!(check.op, Op::Eq | Op::Ne) {
                return Err(format!("check {}: true/false takes == or !=", check.id));
            }
            let ordered = matches!(check.op, Op::Le | Op::Lt | Op::Ge | Op::Gt);
            if matches!(check.kind, Kind::Max | Kind::SlopePerHour) && !ordered {
                return Err(format!(
                    "check {}: {:?} needs <, <=, > or >=",
                    check.id, check.kind
                ));
            }
        }
        Ok(trial)
    }
}
