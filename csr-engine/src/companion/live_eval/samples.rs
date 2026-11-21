//! The samples file: one JSON object per line, each with a time.
use chrono::DateTime;
use serde_json::Value;

#[derive(Clone, Debug)]
pub struct Sample {
    /// Seconds since the epoch.
    pub at: f64,
    pub row: Value,
}

/// Read the lines that parse and carry a time, oldest first. A line cut short by a writer still
/// at work is skipped, never fatal; a file with text and no readable line is an error.
pub fn parse(text: &str, time_field: &str) -> Result<Vec<Sample>, String> {
    let mut out = vec![];
    let mut lines = 0;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        lines += 1;
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(at) = row[time_field]
            .as_str()
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        else {
            continue;
        };
        out.push(Sample {
            at: at.timestamp() as f64,
            row,
        });
    }
    if lines > 0 && out.is_empty() {
        return Err(format!("none of the {lines} lines is a sample with a time"));
    }
    out.sort_by(|a, b| a.at.total_cmp(&b.at));
    Ok(out)
}

/// The value at a dotted path; a number steps into a list. `null` is no value.
pub fn lookup<'a>(row: &'a Value, path: &str) -> Option<&'a Value> {
    let mut at = row;
    for step in path.split('.') {
        at = match at {
            Value::Array(list) => list.get(step.parse::<usize>().ok()?)?,
            Value::Object(map) => map.get(step)?,
            _ => return None,
        };
    }
    (!at.is_null()).then_some(at)
}
