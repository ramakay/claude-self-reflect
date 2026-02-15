use std::collections::BTreeMap;

use anyhow::{anyhow, Result};
use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, Utc};

use crate::import::ConversationChunk;

/// Parse a timestamp string that may or may not have a timezone designator.
/// Handles RFC 3339 (with `Z` or offset) and bare ISO 8601 (assumes UTC).
pub fn parse_timestamp(s: &str) -> Option<DateTime<Utc>> {
    // Try RFC 3339 first (has timezone like Z or +00:00)
    if let Ok(ts) = s.parse::<DateTime<Utc>>() {
        return Some(ts);
    }
    // Fall back to NaiveDateTime (no timezone, assume UTC)
    if let Ok(naive) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f") {
        return Some(naive.and_utc());
    }
    if let Ok(naive) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
        return Some(naive.and_utc());
    }
    None
}

/// Parse a natural language or ISO time expression into a (start, end) UTC range.
pub fn parse_time_expression(expr: &str) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
    let now = Utc::now();
    let today_start = now
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("midnight is always valid")
        .and_utc();
    let expr = expr.trim().to_lowercase();

    match expr.as_str() {
        "today" => Ok((today_start, now)),
        "yesterday" => {
            let start = today_start - Duration::days(1);
            Ok((start, today_start))
        }
        "this week" => {
            let days_since_monday = now.weekday().num_days_from_monday() as i64;
            let start = today_start - Duration::days(days_since_monday);
            Ok((start, now))
        }
        "last week" => {
            let days_since_monday = now.weekday().num_days_from_monday() as i64;
            let this_monday = today_start - Duration::days(days_since_monday);
            let last_monday = this_monday - Duration::days(7);
            Ok((last_monday, this_monday))
        }
        "this month" => {
            let start = NaiveDate::from_ymd_opt(now.year(), now.month(), 1)
                .expect("first of month is always valid")
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc();
            Ok((start, now))
        }
        "last month" => {
            let this_month_start = NaiveDate::from_ymd_opt(now.year(), now.month(), 1)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc();
            let last_month = if now.month() == 1 {
                NaiveDate::from_ymd_opt(now.year() - 1, 12, 1)
            } else {
                NaiveDate::from_ymd_opt(now.year(), now.month() - 1, 1)
            };
            let start = last_month
                .expect("first of previous month is always valid")
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc();
            Ok((start, this_month_start))
        }
        _ => parse_dynamic_expression(&expr, now, today_start),
    }
}

fn parse_dynamic_expression(
    expr: &str,
    now: DateTime<Utc>,
    today_start: DateTime<Utc>,
) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
    // "past N days/weeks/months" / "last N days/weeks/months"
    for prefix in &["past ", "last "] {
        if expr.starts_with(prefix) {
            let rest = &expr[prefix.len()..];
            if let Some(n_str) = rest.strip_suffix(" days") {
                if let Ok(n) = n_str.trim().parse::<i64>() {
                    return Ok((now - Duration::days(n), now));
                }
            }
            if let Some(n_str) = rest.strip_suffix(" weeks") {
                if let Ok(n) = n_str.trim().parse::<i64>() {
                    return Ok((now - Duration::weeks(n), now));
                }
            }
            if let Some(n_str) = rest
                .strip_suffix(" months")
                .or_else(|| rest.strip_suffix(" month"))
            {
                if let Ok(n) = n_str.trim().parse::<u32>() {
                    let n = n.min(1200); // Cap at 100 years to prevent DoS
                    let total_months = now.year() as i32 * 12 + now.month() as i32 - 1 - n as i32;
                    let year = total_months.div_euclid(12);
                    let month = (total_months.rem_euclid(12) + 1) as u32;
                    let start = NaiveDate::from_ymd_opt(year, month, 1)
                        .unwrap_or_else(|| now.date_naive())
                        .and_hms_opt(0, 0, 0)
                        .unwrap()
                        .and_utc();
                    return Ok((start, now));
                }
            }
        }
    }

    // "N days ago"
    if let Some(rest) = expr.strip_suffix(" days ago") {
        if let Ok(n) = rest.trim().parse::<i64>() {
            let target = today_start - Duration::days(n);
            let end = target + Duration::days(1);
            return Ok((target, end));
        }
    }

    // ISO 8601 datetime (RFC 3339)
    if let Ok(ts) = DateTime::parse_from_rfc3339(expr) {
        return Ok((ts.with_timezone(&Utc), now));
    }

    // ISO date (YYYY-MM-DD)
    if let Ok(date) = NaiveDate::parse_from_str(expr, "%Y-%m-%d") {
        let start = date.and_hms_opt(0, 0, 0).expect("midnight is always valid").and_utc();
        let end = start + Duration::days(1);
        return Ok((start, end));
    }

    Err(anyhow!("Could not parse time expression: '{}'", expr))
}

/// Format a timestamp as relative time (e.g., "2 hours ago", "yesterday").
pub fn format_relative_time(ts: &DateTime<Utc>) -> String {
    let now = Utc::now();
    let diff = now - *ts;
    let hours = diff.num_hours();
    let days = diff.num_days();

    if hours < 1 {
        let mins = diff.num_minutes();
        if mins < 1 {
            "just now".to_string()
        } else {
            format!("{}m ago", mins)
        }
    } else if hours < 24 {
        format!("{}h ago", hours)
    } else if days == 1 {
        "yesterday".to_string()
    } else if days < 7 {
        format!("{}d ago", days)
    } else if days < 30 {
        format!("{}w ago", days / 7)
    } else {
        format!("{}mo ago", days / 30)
    }
}

/// Group chunks by time period. Returns ordered map of period key → chunks.
pub fn group_chunks_by_period<'a>(
    chunks: &'a [ConversationChunk],
    granularity: &str,
) -> BTreeMap<String, Vec<&'a ConversationChunk>> {
    let mut groups: BTreeMap<String, Vec<&ConversationChunk>> = BTreeMap::new();

    for chunk in chunks {
        let key = if let Some(ts) = parse_timestamp(&chunk.timestamp) {
            match granularity {
                "hour" => ts.format("%Y-%m-%d %H:00").to_string(),
                "day" => ts.format("%Y-%m-%d").to_string(),
                "week" => {
                    let iso_week = ts.iso_week();
                    format!("{}-W{:02}", iso_week.year(), iso_week.week())
                }
                "month" => ts.format("%Y-%m").to_string(),
                _ => ts.format("%Y-%m-%d").to_string(),
            }
        } else {
            "unknown".to_string()
        };

        groups.entry(key).or_default().push(chunk);
    }

    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_today() {
        let (start, end) = parse_time_expression("today").unwrap();
        assert!(start <= end);
        assert_eq!(start.date_naive(), Utc::now().date_naive());
    }

    #[test]
    fn test_parse_yesterday() {
        let (start, end) = parse_time_expression("yesterday").unwrap();
        let yesterday = (Utc::now() - Duration::days(1)).date_naive();
        assert_eq!(start.date_naive(), yesterday);
        assert!(start < end);
    }

    #[test]
    fn test_parse_past_n_days() {
        let (start, end) = parse_time_expression("past 7 days").unwrap();
        let diff = end - start;
        assert!((diff.num_days() - 7).abs() <= 1);
    }

    #[test]
    fn test_parse_last_n_days() {
        let (start, end) = parse_time_expression("last 30 days").unwrap();
        let diff = end - start;
        assert!((diff.num_days() - 30).abs() <= 1);
    }

    #[test]
    fn test_parse_n_days_ago() {
        let (start, _end) = parse_time_expression("3 days ago").unwrap();
        let target = (Utc::now() - Duration::days(3)).date_naive();
        assert_eq!(start.date_naive(), target);
    }

    #[test]
    fn test_parse_iso_date() {
        let (start, end) = parse_time_expression("2026-01-15").unwrap();
        assert_eq!(
            start.date_naive(),
            NaiveDate::from_ymd_opt(2026, 1, 15).unwrap()
        );
        assert_eq!((end - start).num_days(), 1);
    }

    #[test]
    fn test_parse_this_week() {
        let result = parse_time_expression("this week");
        assert!(result.is_ok());
        let (start, end) = result.unwrap();
        assert!(start <= end);
    }

    #[test]
    fn test_parse_last_week() {
        let result = parse_time_expression("last week");
        assert!(result.is_ok());
        let (start, end) = result.unwrap();
        assert!((end - start).num_days() == 7);
    }

    #[test]
    fn test_format_relative_time_recent() {
        let now = Utc::now();
        assert_eq!(format_relative_time(&now), "just now");
    }

    #[test]
    fn test_format_relative_time_days() {
        let ts = Utc::now() - Duration::days(3);
        assert_eq!(format_relative_time(&ts), "3d ago");
    }

    #[test]
    fn test_format_relative_time_yesterday() {
        // Use 36 hours to be safe regardless of time of day
        let ts = Utc::now() - Duration::hours(36);
        assert_eq!(format_relative_time(&ts), "yesterday");
    }

    #[test]
    fn test_group_by_day() {
        let chunks = vec![
            ConversationChunk {
                id: "1".into(),
                conversation_id: "c1".into(),
                project_name: "test".into(),
                timestamp: "2026-01-15T10:00:00Z".into(),
                content: "hello".into(),
                message_count: 1,
            },
            ConversationChunk {
                id: "2".into(),
                conversation_id: "c2".into(),
                project_name: "test".into(),
                timestamp: "2026-01-15T14:00:00Z".into(),
                content: "world".into(),
                message_count: 1,
            },
            ConversationChunk {
                id: "3".into(),
                conversation_id: "c3".into(),
                project_name: "test".into(),
                timestamp: "2026-01-16T10:00:00Z".into(),
                content: "other".into(),
                message_count: 1,
            },
        ];

        let groups = group_chunks_by_period(&chunks, "day");
        assert_eq!(groups.len(), 2);
        assert_eq!(groups["2026-01-15"].len(), 2);
        assert_eq!(groups["2026-01-16"].len(), 1);
    }

    #[test]
    fn test_parse_last_n_weeks() {
        let (start, end) = parse_time_expression("last 2 weeks").unwrap();
        let diff = end - start;
        assert!((diff.num_days() - 14).abs() <= 1);
    }

    #[test]
    fn test_parse_past_n_weeks() {
        let (start, end) = parse_time_expression("past 3 weeks").unwrap();
        let diff = end - start;
        assert!((diff.num_days() - 21).abs() <= 1);
    }

    #[test]
    fn test_parse_last_n_months() {
        let (start, end) = parse_time_expression("last 6 months").unwrap();
        assert!(start < end);
        // Should be roughly 180 days
        let diff = end - start;
        assert!(diff.num_days() > 150 && diff.num_days() < 200);
    }

    #[test]
    fn test_parse_last_1_month_singular() {
        let (start, end) = parse_time_expression("last 1 month").unwrap();
        assert!(start < end);
        let diff = end - start;
        // "last 1 month" starts on the 1st of the previous month, so range is 28-62 days
        assert!(diff.num_days() >= 28 && diff.num_days() <= 62);
    }

    #[test]
    fn test_parse_invalid() {
        assert!(parse_time_expression("gobbledygook").is_err());
    }

    #[test]
    fn test_parse_timestamp_rfc3339() {
        let ts = parse_timestamp("2026-01-15T10:30:00Z");
        assert!(ts.is_some());
        assert_eq!(ts.unwrap().year(), 2026);
    }

    #[test]
    fn test_parse_timestamp_no_timezone() {
        // Qdrant-imported timestamps often lack Z suffix
        let ts = parse_timestamp("2025-10-23T04:22:13.723731");
        assert!(ts.is_some());
        let dt = ts.unwrap();
        assert_eq!(dt.year(), 2025);
        assert_eq!(dt.month(), 10);
    }

    #[test]
    fn test_parse_timestamp_no_fractional() {
        let ts = parse_timestamp("2026-01-15T10:30:00");
        assert!(ts.is_some());
    }

    #[test]
    fn test_parse_timestamp_invalid() {
        assert!(parse_timestamp("not a timestamp").is_none());
        assert!(parse_timestamp("").is_none());
    }

    #[test]
    fn test_group_by_day_mixed_timestamps() {
        // Mix of Z-suffixed and bare timestamps
        let chunks = vec![
            ConversationChunk {
                id: "1".into(),
                conversation_id: "c1".into(),
                project_name: "test".into(),
                timestamp: "2026-01-15T10:00:00Z".into(),
                content: "with Z".into(),
                message_count: 1,
            },
            ConversationChunk {
                id: "2".into(),
                conversation_id: "c2".into(),
                project_name: "test".into(),
                timestamp: "2026-01-15T14:00:00.123456".into(),
                content: "without Z".into(),
                message_count: 1,
            },
        ];

        let groups = group_chunks_by_period(&chunks, "day");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups["2026-01-15"].len(), 2);
        assert!(!groups.contains_key("unknown"));
    }
}
