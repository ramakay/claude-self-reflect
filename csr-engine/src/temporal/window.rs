//! How a searched time window is told to the caller: its exact bounds, and the
//! next wider window to try when the first one holds nothing worth showing.

use chrono::{DateTime, Utc};

fn stamp(at: &DateTime<Utc>) -> String {
    at.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// The bounds a recency search used, to the second in UTC, as the result header
/// prints them. `expr` is the `time_range` the caller wrote, when there was
/// one: a calendar window says what it means, so "last month" is never taken
/// for "the last 30 days". The text holds no quote marks (it sits in an XML
/// attribute).
pub fn describe_window(expr: Option<&str>, start: &DateTime<Utc>, end: &DateTime<Utc>) -> String {
    let bounds = format!("{} to {} UTC", stamp(start), stamp(end));
    let meaning = match expr.map(|e| e.trim().to_lowercase()).as_deref() {
        Some("last month") => "last month = the previous calendar month, not the last 30 days",
        Some("last week") => "last week = the previous calendar week, Monday to Monday",
        Some("yesterday") => "yesterday = the previous calendar day",
        Some("this month") => "this month = from the first of the month until now",
        Some("this week") => "this week = from Monday until now",
        _ => return bounds,
    };
    format!("{bounds} ({meaning}; nothing after the end is searched)")
}

/// The expression for the next wider window than `expr`, which the same parser
/// reads, or `None` when there is none to name (a date, a range, or a year
/// already). "last month" widens to "last 2 months", which runs from the first
/// of the month before last until now and so also holds the days since the
/// month turned.
pub fn next_wider_expression(expr: &str) -> Option<String> {
    let expr = expr.trim().to_lowercase();
    match expr.as_str() {
        "today" | "yesterday" => return Some("last 7 days".into()),
        "this week" | "last week" => return Some("last 30 days".into()),
        "this month" | "last month" => return Some("last 2 months".into()),
        _ => {}
    }
    let rest = expr
        .strip_prefix("last ")
        .or_else(|| expr.strip_prefix("past "))?;
    let (count, unit) = rest.split_once(' ')?;
    let count: i64 = count.trim().parse().ok()?;
    let wider = match unit.trim().trim_end_matches('s') {
        "hour" if count <= 24 => "last 7 days",
        "hour" => "last 30 days",
        "day" if count <= 7 => "last 30 days",
        "day" if count <= 30 => "last 90 days",
        "day" if count <= 90 => "last 12 months",
        "week" if count <= 4 => "last 3 months",
        "week" if count <= 40 => "last 12 months",
        "month" if count < 12 => "last 12 months",
        _ => return None,
    };
    Some(wider.into())
}
