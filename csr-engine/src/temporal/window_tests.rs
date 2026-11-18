use super::*;
use chrono::TimeZone;

fn on_17_november() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 17, 18, 30, 0).unwrap()
}

#[test]
fn last_month_is_the_previous_calendar_month_and_the_header_says_so() {
    let now = on_17_november();
    let (start, end) = parse_time_expression_at("last month", now).unwrap();
    assert_eq!(start, Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap());
    assert_eq!(end, Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap());
    // A session of 2 November is outside it.
    let session = Utc.with_ymd_and_hms(2026, 11, 2, 9, 0, 0).unwrap();
    assert!(session >= end);
    let text = describe_window(Some("Last Month"), &start, &end);
    assert!(
        text.starts_with("2026-10-01T00:00:00Z to 2026-11-01T00:00:00Z UTC"),
        "{text}"
    );
    assert!(text.contains("previous calendar month"), "{text}");
    assert!(text.contains("nothing after the end is searched"), "{text}");
    assert!(
        !text.contains('\''),
        "no quote mark in an attribute: {text}"
    );
}

#[test]
fn a_rolling_or_dated_window_prints_bounds_without_a_calendar_note() {
    let now = on_17_november();
    let (start, end) = parse_time_expression_at("last 7 days", now).unwrap();
    assert_eq!(
        describe_window(Some("last 7 days"), &start, &end),
        "2026-11-10T18:30:00Z to 2026-11-17T18:30:00Z UTC"
    );
    assert_eq!(
        describe_window(None, &start, &end),
        "2026-11-10T18:30:00Z to 2026-11-17T18:30:00Z UTC"
    );
}

#[test]
fn the_next_wider_window_for_last_month_holds_the_days_since_the_month_turned() {
    let now = on_17_november();
    let wider = next_wider_expression("last month").unwrap();
    assert_eq!(wider, "last 2 months");
    let (start, end) = parse_time_expression_at(&wider, now).unwrap();
    assert_eq!(start, Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap());
    assert_eq!(end, now);
    let session = Utc.with_ymd_and_hms(2026, 11, 2, 9, 0, 0).unwrap();
    assert!(session >= start && session <= end);
}

#[test]
fn every_named_wider_window_parses_and_is_strictly_wider() {
    let now = on_17_november();
    for expr in [
        "today",
        "yesterday",
        "this week",
        "last week",
        "this month",
        "last month",
        "last 12 hours",
        "last 3 days",
        "past 14 days",
        "last 60 days",
        "last 2 weeks",
        "last 3 months",
    ] {
        let wider = next_wider_expression(expr).unwrap_or_else(|| panic!("{expr}"));
        let (s0, e0) = parse_time_expression_at(expr, now).unwrap();
        let (s1, e1) = parse_time_expression_at(&wider, now).unwrap();
        assert!(s1 <= s0 && e1 >= e0, "{expr} -> {wider}");
        assert!(s1 < s0 || e1 > e0, "{expr} -> {wider}");
    }
    for none in ["last 12 months", "2026-11-01", "gobbledygook"] {
        assert_eq!(next_wider_expression(none), None, "{none}");
    }
}
