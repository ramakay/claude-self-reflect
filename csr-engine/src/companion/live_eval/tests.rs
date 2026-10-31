use super::*;
use chrono::Utc;
use serde_json::{json, Value};
use std::fs;

const START: &str = "2026-10-27T23:00:00Z";
const END: &str = "2026-10-28T07:00:00Z";

fn at(t: &str) -> f64 {
    chrono::DateTime::parse_from_rfc3339(t).unwrap().timestamp() as f64
}
fn check(id: &str, label: &str, metric: &str, op: &str, threshold: Value, kind: &str) -> Value {
    json!({"id":id,"label":label,"metric":metric,"op":op,"threshold":threshold,"kind":kind})
}
fn trial_with(checks: Vec<Value>) -> Trial {
    Trial::from_json(
        &json!({"name":"Combo soak","started_at":START,"planned_end":END,
            "candidate":{"label":"combo","sha":"ac9c24c3"},"source":"monitor.log","checks":checks})
        .to_string(),
    )
    .unwrap()
}
fn samples(rows: &[(&str, Value)]) -> Vec<Sample> {
    let text: String = rows
        .iter()
        .map(|(t, v)| {
            let mut v = v.clone();
            v["t"] = json!(t);
            v.to_string() + "\n"
        })
        .collect();
    parse_samples(&text, "t").unwrap()
}
fn one(check: Value, rows: &[(&str, Value)], now: &str) -> CheckResult {
    let report = evaluate(&trial_with(vec![check]), &samples(rows), at(now));
    report.checks[0].clone()
}

#[test]
fn latest_compares_the_last_sample_to_the_threshold() {
    let c = check("e", "Daemon errors", "errors", "==", json!(0), "latest");
    let ok = one(
        c.clone(),
        &[
            ("2026-10-28T00:00:00Z", json!({"errors":3})),
            ("2026-10-28T00:10:00Z", json!({"errors":0})),
        ],
        "2026-10-28T00:12:00Z",
    );
    assert_eq!((ok.status, ok.value), (Status::Pass, Some(Val::Num(0.))));
    let bad = one(
        c,
        &[
            ("2026-10-28T00:00:00Z", json!({"errors":0})),
            ("2026-10-28T00:10:00Z", json!({"errors":2})),
        ],
        "2026-10-28T00:12:00Z",
    );
    assert_eq!((bad.status, bad.value), (Status::Fail, Some(Val::Num(2.))));
}

#[test]
fn latest_reads_true_or_false_and_nested_paths() {
    let c = check(
        "r",
        "Replay finished",
        "hooks.done",
        "==",
        json!(true),
        "latest",
    );
    let rows = [("2026-10-28T00:00:00Z", json!({"hooks":{"done":false}}))];
    assert_eq!(
        one(c.clone(), &rows, "2026-10-28T00:01:00Z").status,
        Status::Fail
    );
    let rows = [("2026-10-28T00:00:00Z", json!({"hooks":{"done":true}}))];
    let r = one(c, &rows, "2026-10-28T00:01:00Z");
    assert_eq!((r.status, r.value), (Status::Pass, Some(Val::Bool(true))));
}

#[test]
fn max_is_the_worst_over_the_run_and_a_list_is_indexed_by_number() {
    let rows = [
        (
            "2026-10-28T00:00:00Z",
            json!({"p":{"prompt":[16.0, 400.0, 500.0]}}),
        ),
        (
            "2026-10-28T00:10:00Z",
            json!({"p":{"prompt":[16.0, 700.0, 800.0]}}),
        ),
        (
            "2026-10-28T00:20:00Z",
            json!({"p":{"prompt":[16.0, 300.0, 800.0]}}),
        ),
    ];
    let limit = |n: f64| check("p", "Prompt p95", "p.prompt.1", "<=", json!(n), "max");
    let fail = one(limit(600.), &rows, "2026-10-28T00:21:00Z");
    assert_eq!(
        (fail.status, fail.value),
        (Status::Fail, Some(Val::Num(700.)))
    );
    let pass = one(limit(800.), &rows, "2026-10-28T00:21:00Z");
    assert_eq!(
        (pass.status, pass.value),
        (Status::Pass, Some(Val::Num(700.)))
    );
    // Against a floor the worst is the lowest.
    let floor = check("f", "Hit rate", "p.prompt.1", ">=", json!(350), "max");
    let r = one(floor, &rows, "2026-10-28T00:21:00Z");
    assert_eq!((r.status, r.value), (Status::Fail, Some(Val::Num(300.))));
}

#[test]
fn a_metric_a_sample_lacks_is_skipped_and_none_at_all_is_pending() {
    let c = check("h", "Hook p95", "hooks.stop.1", "<=", json!(100), "max");
    let rows = [
        ("2026-10-28T00:00:00Z", json!({"hooks":{}})),
        (
            "2026-10-28T00:10:00Z",
            json!({"hooks":{"stop":[1.0, null]}}),
        ),
    ];
    let r = one(c, &rows, "2026-10-28T00:11:00Z");
    assert_eq!((r.status, r.value), (Status::Pending, None));
}

fn rss(hours: &[(&str, f64)]) -> Vec<(String, Value)> {
    hours
        .iter()
        .map(|(t, v)| ((*t).to_string(), json!({"rss":v})))
        .collect()
}
fn slope_check(limit: f64) -> Value {
    let mut c = check(
        "m",
        "Memory trend",
        "rss",
        "<=",
        json!(limit),
        "slope_per_hour",
    );
    c["warmup_hours"] = json!(1.0);
    c["min_span_hours"] = json!(0.0);
    c["unit"] = json!("MB/h");
    c
}

#[test]
fn slope_is_least_squares_per_hour_after_the_warm_up() {
    // Before 00:00 the warm-up hour is left out: the 9000 never counts.
    let rows = rss(&[
        ("2026-10-27T23:30:00Z", 9000.),
        ("2026-10-28T00:00:00Z", 1000.),
        ("2026-10-28T01:00:00Z", 1200.),
        ("2026-10-28T02:00:00Z", 1400.),
    ]);
    let rows: Vec<(&str, Value)> = rows.iter().map(|(t, v)| (t.as_str(), v.clone())).collect();
    let r = one(slope_check(150.), &rows, "2026-10-28T02:05:00Z");
    assert_eq!(r.status, Status::Fail);
    let Some(Val::Num(slope)) = r.value else {
        panic!("{r:?}")
    };
    assert!((slope - 200.).abs() < 1e-6, "{slope}");
    assert_eq!(
        one(slope_check(250.), &rows, "2026-10-28T02:05:00Z").status,
        Status::Pass
    );
}

#[test]
fn slope_waits_for_three_samples_after_the_warm_up() {
    let rows = [
        ("2026-10-27T23:10:00Z", json!({"rss":5000})),
        ("2026-10-28T00:10:00Z", json!({"rss":5000})),
        ("2026-10-28T00:20:00Z", json!({"rss":5100})),
    ];
    let r = one(slope_check(150.), &rows, "2026-10-28T00:21:00Z");
    assert_eq!((r.status, r.value), (Status::Pending, None));
}

/// The live card's check: warm-up 1 h, limit +150 MB/h, and the default span of 6 h.
fn card_slope_check() -> Value {
    let mut c = slope_check(150.);
    c.as_object_mut().unwrap().remove("min_span_hours");
    c
}
/// One sample every `step` minutes from `from_min` for `count` samples, from the soak's start.
fn rss_run(
    from_min: i64,
    step: i64,
    count: i64,
    value: impl Fn(f64) -> f64,
) -> Vec<(String, Value)> {
    let start = at(START);
    (0..count)
        .map(|i| {
            let t = start + (from_min + i * step) as f64 * 60.;
            let hours = (t - start) / 3600.;
            let when = chrono::DateTime::from_timestamp(t as i64, 0)
                .unwrap()
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string();
            (when, json!({"rss": value(hours)}))
        })
        .collect()
}
fn borrowed(rows: &[(String, Value)]) -> Vec<(&str, Value)> {
    rows.iter().map(|(t, v)| (t.as_str(), v.clone())).collect()
}

#[test]
fn slope_waits_until_its_samples_span_the_minimum() {
    // The 10-29 card: 5 samples over 40 minutes after the warm-up, climbing steeply.
    let all = rss_run(60, 10, 5, |h| 1800. + 1200. * h);
    let r = one(card_slope_check(), &borrowed(&all), "2026-10-28T00:45:00Z");
    assert_eq!((r.status, r.value), (Status::Pending, None));
    let (have, need) = r.short_span.unwrap();
    assert!(
        (have - 40. / 60.).abs() < 1e-6 && need == 6.,
        "{have} {need}"
    );
}

#[test]
fn slope_passes_flat_or_noisy_samples_once_the_span_is_held() {
    let flat = rss_run(60, 20, 22, |_| 2700.);
    let r = one(card_slope_check(), &borrowed(&flat), "2026-10-28T07:00:00Z");
    assert_eq!(r.status, Status::Pass);
    // RSS swings 213-6766 MB as caches load and drop: no trend, so no fail.
    let noisy = rss_run(60, 20, 22, |h| {
        if (h * 3.) as i64 % 2 == 0 {
            6000.
        } else {
            400.
        }
    });
    let r = one(
        card_slope_check(),
        &borrowed(&noisy),
        "2026-10-28T07:00:00Z",
    );
    assert_eq!(r.status, Status::Pass, "{r:?}");
    assert!(r.short_span.is_none());
}

#[test]
fn slope_fails_a_steady_rise_over_the_limit_once_the_span_is_held() {
    let rise = rss_run(60, 20, 22, |h| 2000. + 300. * h);
    let r = one(card_slope_check(), &borrowed(&rise), "2026-10-28T07:00:00Z");
    assert_eq!(r.status, Status::Fail);
    let Some(Val::Num(slope)) = r.value else {
        panic!("{r:?}")
    };
    assert!((slope - 300.).abs() < 1e-6, "{slope}");
    // Just under the span it still waits, however steep.
    let short = rss_run(60, 20, 17, |h| 2000. + 300. * h);
    let r = one(
        card_slope_check(),
        &borrowed(&short),
        "2026-10-28T06:00:00Z",
    );
    assert_eq!(r.status, Status::Pending);
}

#[test]
fn slope_counts_an_exact_span_and_never_says_it_holds_what_it_lacks() {
    // Samples 1 h 59 m and 7 h 59 m in: exactly 6 h apart, though offset from the warm-up.
    let rows = [
        ("2026-10-28T00:59:00Z", json!({"rss":2000})),
        ("2026-10-28T04:00:00Z", json!({"rss":2000})),
        ("2026-10-28T06:59:00Z", json!({"rss":2000})),
    ];
    let mut c = card_slope_check();
    c["warmup_hours"] = json!(0.0);
    let r = one(c.clone(), &rows, "2026-10-28T06:59:30Z");
    assert!(r.short_span.is_none(), "{r:?}");
    assert_eq!(r.status, Status::Pass);
    // 5 h 59 m 30 s still waits, and the line does not round itself up to "have 6 h".
    let rows = [
        ("2026-10-28T01:00:00Z", json!({"rss":2000})),
        ("2026-10-28T04:00:00Z", json!({"rss":2000})),
        ("2026-10-28T06:59:30Z", json!({"rss":2000})),
    ];
    let report = evaluate(
        &trial_with(vec![c]),
        &samples(&rows),
        at("2026-10-28T06:59:40Z"),
    );
    assert_eq!(report.checks[0].status, Status::Pending);
    assert_eq!(
        card(&report, &Utc).rows[0].text,
        "Memory trend: waiting for 6 h of samples (have 5.9 h)"
    );
}

#[test]
fn the_waiting_line_says_why_in_words() {
    let rows = rss_run(60, 20, 3, |h| 1800. + 1200. * h);
    let report = evaluate(
        &trial_with(vec![card_slope_check()]),
        &samples(&borrowed(&rows)),
        at("2026-10-28T00:45:00Z"),
    );
    let card = card(&report, &Utc);
    assert_eq!(card.rows[0].mark, "…");
    assert_eq!(
        card.rows[0].text,
        "Memory trend: waiting for 6 h of samples (have 0.7 h)"
    );
    assert_eq!(
        card.line1,
        "0 of 1 checks passing · 1 waiting · Waiting for data"
    );
    // A trial that ends short of the span says so rather than "had no data".
    let ended = evaluate(
        &trial_with(vec![card_slope_check()]),
        &samples(&borrowed(&rows)),
        at(END),
    );
    assert_eq!(
        super::card(&ended, &Utc).rows[0].text,
        "Memory trend: only 0.7 h of samples, needed 6 h"
    );
}

#[test]
fn at_least_by_end_is_pending_until_met_and_fails_only_at_the_end() {
    let c = check(
        "d",
        "Dream cycles",
        "dreams",
        ">=",
        json!(1),
        "at_least_by_end",
    );
    let none = [("2026-10-28T06:50:00Z", json!({"dreams":0}))];
    let before = one(c.clone(), &none, "2026-10-28T06:59:00Z");
    assert_eq!(
        (before.status, before.value),
        (Status::Pending, Some(Val::Num(0.)))
    );
    let after = one(c.clone(), &none, END);
    assert_eq!(after.status, Status::Fail);
    // Met once, met for good: a later dip does not undo it.
    let met = [
        ("2026-10-28T03:00:00Z", json!({"dreams":1})),
        ("2026-10-28T06:50:00Z", json!({"dreams":0})),
    ];
    assert_eq!(one(c, &met, "2026-10-28T06:59:00Z").status, Status::Pass);
}

#[test]
fn samples_after_the_planned_end_are_not_counted() {
    let c = check("e", "Daemon errors", "errors", "==", json!(0), "latest");
    let rows = [
        ("2026-10-28T06:50:00Z", json!({"errors":0})),
        ("2026-10-28T07:30:00Z", json!({"errors":9})),
    ];
    assert_eq!(one(c, &rows, "2026-10-28T08:00:00Z").status, Status::Pass);
}

fn trend_of(values: &[f64]) -> Option<Trend> {
    let rows: Vec<(String, Value)> = values
        .iter()
        .enumerate()
        .map(|(i, v)| (format!("2026-10-28T00:{:02}:00Z", i * 10), json!({"x":v})))
        .collect();
    let rows: Vec<(&str, Value)> = rows.iter().map(|(t, v)| (t.as_str(), v.clone())).collect();
    one(
        check("x", "X", "x", "<=", json!(1000), "latest"),
        &rows,
        "2026-10-28T01:00:00Z",
    )
    .trend
}

#[test]
fn the_trend_arrow_comes_from_the_last_three_samples() {
    assert_eq!(trend_of(&[100., 200., 300.]), Some(Trend::Up));
    assert_eq!(trend_of(&[300., 200., 100.]), Some(Trend::Down));
    assert_eq!(trend_of(&[100., 100., 100.]), Some(Trend::Flat));
    // Only the last three count: the early fall is forgotten.
    assert_eq!(trend_of(&[900., 100., 100., 101., 100.]), Some(Trend::Flat));
    // A change under one percent is flat.
    assert_eq!(trend_of(&[1000., 1002., 1004.]), Some(Trend::Flat));
    assert_eq!(trend_of(&[100.]), None);
}

fn three_checks() -> Vec<Value> {
    vec![
        check("e", "Daemon errors", "errors", "==", json!(0), "latest"),
        check("p", "Prompt hook p95", "p", "<=", json!(600), "max"),
        check(
            "d",
            "Dream cycles",
            "dreams",
            ">=",
            json!(1),
            "at_least_by_end",
        ),
    ]
}
fn row(errors: u32, p: f64, dreams: u32) -> Value {
    json!({"errors":errors,"p":p,"dreams":dreams})
}

#[test]
fn the_verdict_counts_passing_failing_and_pending_checks() {
    let rows = [
        ("2026-10-28T00:00:00Z", row(0, 400., 0)),
        ("2026-10-28T00:10:00Z", row(0, 450., 0)),
    ];
    let r = evaluate(
        &trial_with(three_checks()),
        &samples(&rows),
        at("2026-10-28T00:12:00Z"),
    );
    assert_eq!((r.passing, r.failing, r.pending), (2, 0, 1));
    assert_eq!(r.headline, Headline::OnTrack);
    assert!(!r.finished);
}

#[test]
fn a_failing_check_names_itself_in_the_headline() {
    let rows = [("2026-10-28T00:00:00Z", row(2, 400., 0))];
    let r = evaluate(
        &trial_with(three_checks()),
        &samples(&rows),
        at("2026-10-28T00:02:00Z"),
    );
    assert_eq!((r.passing, r.failing, r.pending), (1, 1, 1));
    assert_eq!(r.headline, Headline::Failing(vec!["Daemon errors".into()]));
}

#[test]
fn no_samples_is_waiting_for_data_not_on_track() {
    let r = evaluate(&trial_with(three_checks()), &[], at("2026-10-28T00:02:00Z"));
    assert_eq!((r.passing, r.failing, r.pending), (0, 0, 3));
    assert_eq!(r.headline, Headline::Waiting);
    assert_eq!(r.stale_for, None);
}

#[test]
fn data_goes_stale_after_twice_the_sample_interval() {
    let rows = [
        ("2026-10-28T00:00:00Z", row(0, 1., 0)),
        ("2026-10-28T00:10:00Z", row(0, 1., 0)),
        ("2026-10-28T00:20:00Z", row(0, 1., 0)),
    ];
    let s = samples(&rows);
    let t = trial_with(three_checks());
    let fresh = evaluate(&t, &s, at("2026-10-28T00:39:00Z"));
    assert_eq!(fresh.interval, 600.);
    assert_eq!(fresh.stale_for, None);
    let stale = evaluate(&t, &s, at("2026-10-28T00:45:00Z"));
    assert_eq!(stale.stale_for, Some(1500.));
}

#[test]
fn the_planned_interval_in_the_spec_beats_the_samples_own_spacing() {
    let mut t = trial_with(three_checks());
    t.sample_interval_secs = Some(60.);
    let s = samples(&[("2026-10-28T00:00:00Z", row(0, 1., 0))]);
    assert_eq!(
        evaluate(&t, &s, at("2026-10-28T00:03:00Z")).stale_for,
        Some(180.)
    );
}

#[test]
fn after_the_end_the_verdict_is_final_and_pending_checks_fail() {
    let rows = [("2026-10-28T06:50:00Z", row(0, 400., 0))];
    let r = evaluate(
        &trial_with(three_checks()),
        &samples(&rows),
        at("2026-10-28T07:10:00Z"),
    );
    assert!(r.finished);
    assert_eq!((r.passing, r.failing, r.pending), (2, 1, 0));
    assert_eq!(r.headline, Headline::FinishedFailed(1));
    assert_eq!(r.stale_for, None, "a finished trial is not stale");
    let rows = [("2026-10-28T06:50:00Z", row(0, 400., 1))];
    let r = evaluate(
        &trial_with(three_checks()),
        &samples(&rows),
        at("2026-10-28T07:10:00Z"),
    );
    assert_eq!(r.headline, Headline::FinishedClean);
}

#[test]
fn a_spec_that_cannot_be_held_is_refused_with_its_reason() {
    let bad = |c: Value| {
        Trial::from_json(
            &json!({"name":"n","started_at":START,"planned_end":END,"source":"s","checks":[c]})
                .to_string(),
        )
        .unwrap_err()
    };
    assert!(bad(check("a", "A", "x", "==", json!(0), "max")).contains("needs <"));
    assert!(bad(check("a", "A", "x", "<=", json!("zero"), "latest")).contains("threshold"));
    assert!(bad(check("a", "A", "x", "<=", json!(true), "latest")).contains("== or !="));
    let reversed = Trial::from_json(
        &json!({"name":"n","started_at":END,"planned_end":START,"source":"s","checks":[]})
            .to_string(),
    );
    assert!(reversed.unwrap_err().contains("planned_end"));
    assert!(Trial::from_json("{not json").is_err());
}

#[test]
fn a_corrupt_samples_file_is_an_error_and_a_cut_last_line_is_not() {
    assert!(parse_samples("nonsense\n{\"x\":1}\n", "t").is_err());
    let text = "{\"t\":\"2026-10-28T00:00:00Z\",\"x\":1}\n{\"t\":\"2026-10-28T00:1";
    assert_eq!(parse_samples(text, "t").unwrap().len(), 1);
    assert_eq!(parse_samples("", "t").unwrap().len(), 0);
}

// ---- words

fn soak_trial() -> Trial {
    let checks = vec![
        check(
            "e",
            "Daemon errors",
            "daemon_errors",
            "==",
            json!(0),
            "latest",
        ),
        {
            let mut c = check(
                "m",
                "Memory trend",
                "rss_mb",
                "<=",
                json!(150),
                "slope_per_hour",
            );
            c["warmup_hours"] = json!(1.0);
            c["min_span_hours"] = json!(0.0);
            c["unit"] = json!("MB/h");
            c
        },
        {
            let mut c = check(
                "p",
                "Prompt hook p95, worst",
                "ph.prompt.1",
                "<=",
                json!(800),
                "max",
            );
            c["unit"] = json!("ms");
            c
        },
        check(
            "r",
            "Replay finished",
            "replay",
            "==",
            json!(true),
            "at_least_by_end",
        ),
        check(
            "d",
            "Dream cycles",
            "dreams",
            ">=",
            json!(1),
            "at_least_by_end",
        ),
    ];
    Trial::from_json(
        &json!({"name":"Combo soak","started_at":"2026-10-27T23:04:50Z","planned_end":"2026-10-28T07:04:50Z",
            "candidate":{"label":"combo","sha":"ac9c24c3"},"source":"monitor.log","checks":checks})
        .to_string(),
    )
    .unwrap()
}
fn soak_rows() -> Vec<Sample> {
    let r = |t: &str, rss: f64, p: f64| {
        (
            t.to_string(),
            json!({"daemon_errors":0,"rss_mb":rss,"ph":{"prompt":[16.0, p, 490.0]},"replay":false,"dreams":0}),
        )
    };
    let rows = [
        r("2026-10-28T00:14:50Z", 6711., 481.1),
        r("2026-10-28T00:24:50Z", 4773., 481.1),
        r("2026-10-28T00:34:50Z", 4730., 481.1),
    ];
    let rows: Vec<(&str, Value)> = rows.iter().map(|(t, v)| (t.as_str(), v.clone())).collect();
    samples(&rows)
}

#[test]
fn the_card_reads_a_big_number_a_headline_the_end_time_and_one_line_per_check() {
    let report = evaluate(&soak_trial(), &soak_rows(), at("2026-10-28T00:42:33Z"));
    let card = card(&report, &Utc);
    assert_eq!(card.title, "CSR performance");
    assert_eq!(card.candidate, "combo ac9c24c3");
    assert_eq!(card.line1, "3 of 5 checks passing · 2 waiting · On track");
    assert_eq!(card.sev, "ok");
    assert_eq!(card.line2, "Combo soak · ends 7:04 AM · 6h 22m left");
    assert!(
        (card.progress - (3600. + 37. * 60. + 43.) / (8. * 3600.)).abs() < 1e-3,
        "{}",
        card.progress
    );
    let lines: Vec<String> = card
        .rows
        .iter()
        .map(|r| format!("{} {}", r.mark, r.text))
        .collect();
    assert_eq!(
        lines,
        [
            "✓ Daemon errors 0 → (limit 0)",
            "✓ Memory trend -5943 MB/h ↓ (limit +150)",
            "✓ Prompt hook p95, worst 481 ms → (limit 800)",
            "… Replay finished: no, need yes by end",
            "… Dream cycles 0, need 1 by end",
        ]
    );
    assert_eq!(card.warning, None);
}

#[test]
fn the_end_names_its_day_when_it_is_not_today() {
    let report = evaluate(&soak_trial(), &soak_rows(), at("2026-10-27T23:30:00Z"));
    assert_eq!(
        card(&report, &Utc).line2,
        "Combo soak · ends Wed 7:04 AM · 7h 34m left"
    );
}

#[test]
fn stale_data_warns_in_words() {
    let report = evaluate(&soak_trial(), &soak_rows(), at("2026-10-28T01:10:00Z"));
    assert_eq!(
        card(&report, &Utc).warning.as_deref(),
        Some("No new data for 35m, expected every 10m")
    );
}

#[test]
fn a_failing_check_is_a_red_cross_with_its_limit() {
    let mut rows = soak_rows();
    rows[2].row["daemon_errors"] = json!(3);
    let report = evaluate(&soak_trial(), &rows, at("2026-10-28T00:42:33Z"));
    let card = card(&report, &Utc);
    assert_eq!(
        card.line1,
        "2 of 5 checks passing · 2 waiting · Failing: Daemon errors"
    );
    assert_eq!(card.sev, "red");
    assert_eq!(card.rows[0].mark, "✗");
    assert_eq!(card.rows[0].sev, "red");
    assert_eq!(card.rows[0].text, "Daemon errors 3 ↑ (limit 0)");
}

#[test]
fn waiting_for_data_says_so_and_each_check_waits() {
    let report = evaluate(&soak_trial(), &[], at("2026-10-27T23:10:00Z"));
    let card = card(&report, &Utc);
    assert_eq!(
        card.line1,
        "0 of 5 checks passing · 5 waiting · Waiting for data"
    );
    assert_eq!(card.rows[0].text, "Daemon errors waiting for data");
    assert_eq!(card.rows[4].text, "Dream cycles no data yet, need 1 by end");
}

#[test]
fn after_the_end_the_card_gives_the_final_verdict() {
    let mut rows = soak_rows();
    for (i, s) in rows.iter_mut().enumerate() {
        s.row["replay"] = json!(true);
        s.row["dreams"] = json!(i);
    }
    let clean = card(
        &evaluate(&soak_trial(), &rows, at("2026-10-28T08:04:50Z")),
        &Utc,
    );
    assert_eq!(clean.line1, "5 of 5 checks passing · Soak finished clean");
    assert_eq!(clean.line2, "Combo soak · ended 7:04 AM · 1h 0m ago");
    assert_eq!(clean.progress, 1.);
    let mut rows = soak_rows();
    rows[2].row["daemon_errors"] = json!(1);
    let failed = card(
        &evaluate(&soak_trial(), &rows, at("2026-10-28T08:04:50Z")),
        &Utc,
    );
    assert_eq!(
        failed.line1,
        "2 of 5 checks passing · Soak finished: 3 failed"
    );
    assert_eq!(failed.sev, "red");
}

#[test]
fn unreadable_data_says_why_and_still_shows_the_clock() {
    let now = at("2026-10-28T00:42:33Z");
    let with = unreadable(Some(&soak_trial()), "monitor.log not found", now, &Utc);
    assert_eq!(with.line1, "Trial data unreadable: monitor.log not found");
    assert_eq!(with.line2, "Combo soak · ends 7:04 AM · 6h 22m left");
    assert!(with.rows.is_empty());
    let without = unreadable(None, "expected value at line 1", now, &Utc);
    assert_eq!(
        without.line1,
        "Trial data unreadable: expected value at line 1"
    );
    assert_eq!(without.line2, "");
}

// ---- load

fn write_trial(dir: &std::path::Path, file: &str, start: &str, end: &str, source: &str) {
    fs::write(
        dir.join(file),
        json!({"name":file,"started_at":start,"planned_end":end,"source":source,
            "checks":[check("e","Daemon errors","errors","==",json!(0),"latest")]})
        .to_string(),
    )
    .unwrap();
}
fn good_source(dir: &std::path::Path) {
    fs::write(
        dir.join("s.log"),
        "{\"t\":\"2026-10-28T00:00:00Z\",\"errors\":0}\n",
    )
    .unwrap();
}

#[test]
fn no_trial_no_card() {
    let home = tempfile::tempdir().unwrap();
    let now = at("2026-10-28T01:00:00Z");
    assert!(card_at(&load(&home.path().join("trials"), now, false), now).is_none());
    fs::create_dir_all(home.path().join("trials")).unwrap();
    assert!(card_at(&load(&home.path().join("trials"), now, false), now).is_none());
}

#[test]
fn a_trial_shows_while_it_runs_and_for_twelve_hours_after() {
    let dir = tempfile::tempdir().unwrap();
    good_source(dir.path());
    write_trial(dir.path(), "a.json", START, END, "s.log");
    let shown = |now: &str| card_at(&load(dir.path(), at(now), false), at(now)).is_some();
    assert!(!shown("2026-10-27T22:00:00Z"), "not started");
    assert!(shown("2026-10-28T01:00:00Z"));
    assert!(shown("2026-10-28T18:59:00Z"), "11h59m after the end");
    assert!(!shown("2026-10-28T19:01:00Z"), "past twelve hours");
}

#[test]
fn the_newest_trial_that_covers_now_is_the_active_one() {
    let dir = tempfile::tempdir().unwrap();
    good_source(dir.path());
    write_trial(
        dir.path(),
        "old.json",
        "2026-10-27T20:00:00Z",
        "2026-10-28T09:00:00Z",
        "s.log",
    );
    write_trial(dir.path(), "new.json", START, END, "s.log");
    let now = at("2026-10-28T01:00:00Z");
    let card = card_at(&load(dir.path(), now, false), now).unwrap();
    assert!(card.line2.starts_with("new.json"), "{}", card.line2);
}

#[test]
fn a_missing_source_is_unreadable_not_a_crash() {
    let dir = tempfile::tempdir().unwrap();
    write_trial(dir.path(), "a.json", START, END, "gone.log");
    let now = at("2026-10-28T01:00:00Z");
    let card = card_at(&load(dir.path(), now, false), now).unwrap();
    assert_eq!(card.line1, "Trial data unreadable: gone.log not found");
    assert!(card.line2.starts_with("a.json · ends"));
}

#[test]
fn a_corrupt_source_is_unreadable_and_an_empty_one_is_waiting() {
    let dir = tempfile::tempdir().unwrap();
    write_trial(dir.path(), "a.json", START, END, "s.log");
    let now = at("2026-10-28T01:00:00Z");
    fs::write(
        dir.path().join("s.log"),
        "\u{0}\u{1}garbage\nmore garbage\n",
    )
    .unwrap();
    let card = card_at(&load(dir.path(), now, false), now).unwrap();
    assert!(
        card.line1
            .starts_with("Trial data unreadable: none of the 2 lines"),
        "{}",
        card.line1
    );
    fs::write(dir.path().join("s.log"), "").unwrap();
    let card = card_at(&load(dir.path(), now, false), now).unwrap();
    assert!(card.line1.ends_with("Waiting for data"), "{}", card.line1);
}

#[test]
fn a_spec_that_will_not_parse_shows_why_while_it_is_fresh() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.json"), "{\"name\": ").unwrap();
    let mtime = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let card = card_at(&load(dir.path(), mtime, false), mtime).unwrap();
    assert!(
        card.line1.starts_with("Trial data unreadable: a.json: "),
        "{}",
        card.line1
    );
    // Long forgotten, it is not the card's business.
    let later = mtime + 13. * 3600.;
    assert!(card_at(&load(dir.path(), later, false), later).is_none());
}

#[test]
fn a_relative_source_is_read_from_the_specs_folder_and_an_absolute_one_as_given() {
    let dir = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    good_source(other.path());
    write_trial(
        dir.path(),
        "a.json",
        START,
        END,
        other.path().join("s.log").to_str().unwrap(),
    );
    let now = at("2026-10-28T01:00:00Z");
    let card = card_at(&load(dir.path(), now, false), now).unwrap();
    assert!(
        card.line1.contains("1 of 1 checks passing"),
        "{}",
        card.line1
    );
}

// ---- the road

const ROAD_START: &str = "2026-10-27T20:00:00Z";
const ROAD_END: &str = "2026-10-30T07:00:00Z";

/// The seven-check road of the 4.5-star trial, with two checks passing.
fn write_road(dir: &std::path::Path, file: &str, track: Option<&str>) {
    let mut spec = json!({"name":"Road to blind 4.5 stars","started_at":ROAD_START,
    "planned_end":ROAD_END,"source":"road.ndjson","candidate":{"label":"rc","sha":"046c4925"},
    "checks":[
        check("gate","Release gate failures","gate_failed","==",json!(0),"latest"),
        check("recall","Recall p50","recall_p50_ms","<=",json!(800),"at_least_by_end"),
        check("recap","Recap judge: recaps worse","recap_worse","==",json!(0),"at_least_by_end"),
        check("stars","Blind Fable rating","blind_stars",">=",json!(4.5),"at_least_by_end"),
    ]});
    if let Some(track) = track {
        spec["track"] = json!(track);
    }
    fs::write(dir.join(file), spec.to_string()).unwrap();
    fs::write(
        dir.join("road.ndjson"),
        "{\"t\":\"2026-10-28T00:00:00Z\",\"gate_failed\":0,\"recall_p50_ms\":700,\"recap_worse\":5,\"blind_stars\":3}\n",
    )
    .unwrap();
}
fn run_and_road(dir: &std::path::Path) {
    good_source(dir);
    write_trial(dir, "run.json", START, END, "s.log");
    write_road(dir, "road.json", Some("road"));
}
const ROAD_NOW: &str = "2026-10-28T00:30:00Z";

#[test]
fn a_spec_without_a_track_is_a_run_trial() {
    assert_eq!(trial_with(vec![]).track, Track::Run);
    let road = Trial::from_json(
        &json!({"name":"r","track":"road","started_at":START,"planned_end":END,"source":"x","checks":[]})
            .to_string(),
    )
    .unwrap();
    assert_eq!(road.track, Track::Road);
    assert!(Trial::from_json(
        &json!({"name":"r","track":"sprint","started_at":START,"planned_end":END,"source":"x","checks":[]})
            .to_string()
    )
    .is_err());
}

#[test]
fn a_road_is_never_shown_with_the_flag_off() {
    let dir = tempfile::tempdir().unwrap();
    run_and_road(dir.path());
    let now = at(ROAD_NOW);
    let card = card_at(&load(dir.path(), now, false), now).unwrap();
    assert!(card.road.is_none());
    assert!(card.line2.starts_with("run.json"), "{}", card.line2);
    // Alone, it is no card at all.
    fs::remove_file(dir.path().join("run.json")).unwrap();
    assert!(card_at(&load(dir.path(), now, false), now).is_none());
}

#[test]
fn a_broken_road_spec_is_not_reported_with_the_flag_off() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("road.json"),
        json!({"name":"r","track":"road","started_at":"nope"}).to_string(),
    )
    .unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    assert!(card_at(&load(dir.path(), now, false), now).is_none());
    let card = card_at(&load(dir.path(), now, true), now).unwrap();
    assert!(
        card.line1.starts_with("Trial data unreadable: road.json: "),
        "{}",
        card.line1
    );
}

#[test]
fn a_half_written_road_spec_is_not_reported_with_the_flag_off() {
    let dir = tempfile::tempdir().unwrap();
    // Not JSON (a trailing comma), but it still names its track.
    for text in [
        r#"{"track":"road","name":"r",}"#,
        "{\n  \"name\": \"r\",\n  \"track\" :\n \"road\",\n",
    ] {
        fs::write(dir.path().join("road.json"), text).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        assert!(
            card_at(&load(dir.path(), now, false), now).is_none(),
            "{text}"
        );
        assert!(
            card_at(&load(dir.path(), now, true), now).is_some(),
            "{text}"
        );
    }
}

#[test]
fn a_half_written_run_spec_is_still_reported_with_the_flag_off() {
    let dir = tempfile::tempdir().unwrap();
    // A legacy run spec, and a run spec that only mentions the road in another field.
    for text in [
        r#"{"name":"r",}"#,
        r#"{"track":"run","name":"road",}"#,
        r#"{"name":"track road","note":"track: road",}"#,
    ] {
        fs::write(dir.path().join("run.json"), text).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let card = card_at(&load(dir.path(), now, false), now).expect(text);
        assert!(
            card.line1.starts_with("Trial data unreadable: run.json: "),
            "{text}: {}",
            card.line1
        );
    }
}

#[test]
fn the_road_is_one_line_under_the_run_card_with_the_flag_on() {
    let dir = tempfile::tempdir().unwrap();
    run_and_road(dir.path());
    let now = at(ROAD_NOW);
    let card = card_at(&load(dir.path(), now, true), now).unwrap();
    assert!(card.line2.starts_with("run.json"), "the run trial leads");
    let road = card.road.expect("the road line");
    assert_eq!(
        road.text,
        "Long road · 2 of 4 · next: Recap judge: recaps worse"
    );
    assert_eq!((road.mark.as_str(), road.sev.as_str()), ("→", "quiet"));
}

#[test]
fn the_road_alone_makes_the_card_and_names_what_is_next() {
    let dir = tempfile::tempdir().unwrap();
    write_road(dir.path(), "road.json", Some("road"));
    let now = at(ROAD_NOW);
    let card = card_at(&load(dir.path(), now, true), now).unwrap();
    assert_eq!(card.title, "CSR performance");
    assert_eq!(
        card.line1,
        "2 of 4 checks passing · next: Recap judge: recaps worse"
    );
    assert!(
        card.line2.starts_with("Road to blind 4.5 stars · ends"),
        "{}",
        card.line2
    );
    assert_eq!(card.rows.len(), 4);
    assert!(card.road.is_none());
}

#[test]
fn next_is_the_first_check_not_passing_and_a_failure_counts() {
    let dir = tempfile::tempdir().unwrap();
    write_road(dir.path(), "road.json", Some("road"));
    // Gate and recall hold; the recap check is the first that does not.
    fs::write(
        dir.path().join("road.ndjson"),
        "{\"t\":\"2026-10-28T00:00:00Z\",\"gate_failed\":0,\"recall_p50_ms\":700,\"recap_worse\":5,\"blind_stars\":3}\n",
    )
    .unwrap();
    let now = at(ROAD_NOW);
    let line = |dir: &std::path::Path| {
        let shown = load(dir, now, true);
        let report = match &shown.road {
            Loaded::Trial {
                trial,
                samples: Ok(s),
            } => evaluate(trial, s, now),
            _ => panic!("no road"),
        };
        road_line(&report)
    };
    assert_eq!(
        line(dir.path()).text,
        "Long road · 2 of 4 · next: Recap judge: recaps worse"
    );
    fs::write(
        dir.path().join("road.ndjson"),
        "{\"t\":\"2026-10-28T00:00:00Z\",\"gate_failed\":1,\"recall_p50_ms\":700,\"recap_worse\":5,\"blind_stars\":3}\n",
    )
    .unwrap();
    let failing = line(dir.path());
    assert_eq!(
        failing.text,
        "Long road · 1 of 4 · next: Release gate failures"
    );
    assert_eq!((failing.mark.as_str(), failing.sev.as_str()), ("✗", "red"));
    fs::write(
        dir.path().join("road.ndjson"),
        "{\"t\":\"2026-10-28T00:00:00Z\",\"gate_failed\":0,\"recall_p50_ms\":700,\"recap_worse\":0,\"blind_stars\":4.6}\n",
    )
    .unwrap();
    let done = line(dir.path());
    assert_eq!(done.text, "Long road · 4 of 4 · all passing");
    assert_eq!(done.mark, "✓");
}

#[test]
fn an_unreadable_road_beside_a_run_card_is_a_line_that_says_so() {
    let dir = tempfile::tempdir().unwrap();
    good_source(dir.path());
    write_trial(dir.path(), "run.json", START, END, "s.log");
    write_road(dir.path(), "road.json", Some("road"));
    fs::remove_file(dir.path().join("road.ndjson")).unwrap();
    let now = at(ROAD_NOW);
    let card = card_at(&load(dir.path(), now, true), now).unwrap();
    assert_eq!(
        card.road.unwrap().text,
        "Long road: Trial data unreadable: road.ndjson not found"
    );
}

#[test]
fn the_newest_road_wins_among_roads_and_does_not_displace_the_run() {
    let dir = tempfile::tempdir().unwrap();
    run_and_road(dir.path());
    // An older road, and a run that started before the newer road: each track keeps its own.
    let mut older: Value =
        serde_json::from_str(&fs::read_to_string(dir.path().join("road.json")).unwrap()).unwrap();
    older["name"] = json!("Old road");
    older["started_at"] = json!("2026-10-26T00:00:00Z");
    fs::write(dir.path().join("old-road.json"), older.to_string()).unwrap();
    let now = at(ROAD_NOW);
    let card = card_at(&load(dir.path(), now, true), now).unwrap();
    assert!(card.line2.starts_with("run.json"));
    assert!(card.road.unwrap().text.starts_with("Long road"));
}
