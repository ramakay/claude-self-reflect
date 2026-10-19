#[cfg(test)]
use super::paths::Paths;
use super::{
    array, num,
    paths::epoch,
    pyfmt::{round, short},
    string,
};
use chrono::{Local, TimeZone};
use serde_json::{json, Value};

pub use super::sources::{
    account_limits, compacting_info, csr_status, csr_status_at, kept_status, threshold,
    usage_fields,
};

pub fn seconds_of(value: &Value) -> Option<f64> {
    let n = value
        .as_f64()
        .map(|v| if v > 1e11 { v / 1000. } else { v })
        .or_else(|| value.as_str().and_then(epoch))?;
    (n.is_finite() && (0.0..253402300000.).contains(&n)).then_some(n)
}
pub fn percent_of(v: &Value) -> Option<f64> {
    v.as_f64().filter(|v| v.is_finite())
}
pub fn ctx_amber() -> f64 {
    threshold("CC_CTX_AMBER_TOKENS", 100000.)
}
pub fn ctx_red() -> f64 {
    threshold("CC_CTX_RED_TOKENS", 400000.)
}
pub fn ctx_hot() -> f64 {
    threshold("CC_CTX_HOT_TOKENS", 200000.)
}
pub fn ctx_now(fields: &Value, at: Option<f64>, s: &Value) -> (Option<f64>, Option<f64>) {
    let w = &fields["context_window"];
    let size = percent_of(&w["context_window_size"]).filter(|n| *n > 0.);
    if let Some(reply) = array(&s["main"])
        .iter()
        .max_by(|a, b| num(&a[0]).total_cmp(&num(&b[0])))
    {
        if num(&reply[0]) > at.unwrap_or(0.) + 5. && num(&reply[1]) > 0. {
            return (reply[1].as_f64(), size);
        }
    }
    let tokens = percent_of(&w["total_input_tokens"]).or_else(|| {
        w["current_usage"].is_object().then(|| {
            [
                "input_tokens",
                "cache_read_input_tokens",
                "cache_creation_input_tokens",
            ]
            .iter()
            .map(|k| num(&w["current_usage"][k]))
            .sum()
        })
    });
    (tokens.filter(|n| *n > 0.), size)
}
pub fn median_of(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut vals = values.to_vec();
    vals.sort_by(f64::total_cmp);
    let i = vals.len() / 2;
    Some(if vals.len() % 2 == 1 {
        vals[i]
    } else {
        (vals[i - 1] + vals[i]) / 2.
    })
}
pub fn median_last(values: &[f64], default: f64) -> (f64, bool) {
    let v: Vec<_> = values
        .iter()
        .copied()
        .filter(|n| n.is_finite() && *n > 0.)
        .collect();
    median_of(&v[v.len().saturating_sub(5)..])
        .map(|n| {
            (
                if v.len() % 2 == 1 {
                    n
                } else {
                    (n + 0.5).floor()
                },
                true,
            )
        })
        .unwrap_or((default, false))
}
pub fn floor_of(sizes: &Value) -> (f64, bool) {
    let values: Vec<_> = array(sizes)
        .iter()
        .filter_map(|row| {
            if row.is_array() {
                row.get(1).and_then(Value::as_f64)
            } else {
                row.as_f64()
            }
        })
        .collect();
    median_last(&values, 65000.)
}
pub fn summary_of(sizes: &Value) -> (f64, bool) {
    let values: Vec<_> = array(sizes).iter().filter_map(Value::as_f64).collect();
    median_last(&values, 21700.)
}
pub fn ttl_seconds(text: &str) -> Option<f64> {
    static TTL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"^\s*(\d+(?:\.\d+)?)\s*([smhd])\s*$").unwrap()
    });
    let c = TTL.captures(text)?;
    let factor = match &c[2] {
        "s" => 1.,
        "m" => 60.,
        "h" => 3600.,
        "d" => 86400.,
        _ => return None,
    };
    let n = c[1].parse::<f64>().ok()? * factor;
    (n > 0. && n <= 86400.).then_some(n)
}
pub fn span(seconds: f64) -> String {
    let h = (seconds / 3600.).floor() as i64;
    let m = (seconds.rem_euclid(3600.) / 60.).floor() as i64;
    if h != 0 {
        format!("{h}h{m:02}m")
    } else {
        format!("{m}m")
    }
}
pub fn cache_info(cache: &Value, now: f64) -> Value {
    let mut out = json!({
        "text": "CACHE --",
        "sev": "normal",
        "long": "--",
        "cold": false,
        "left": null,
        "ttl": null,
    });
    if !cache.is_object() {
        return out;
    }
    let mut expires = seconds_of(&cache["expires_at"]);
    let cost = percent_of(&cache["recache_tokens_if_cold"])
        .filter(|n| *n > 0.)
        .map(|n| format!(" ~{}", short(n)))
        .unwrap_or_default();
    if cache["warm"] == false || expires.is_some_and(|n| n <= now) {
        out["text"] = json!(format!("CACHE COLD{cost}"));
        out["sev"] = json!("amber");
        out["long"] = json!(format!(
            "COLD{}",
            if cost.is_empty() {
                String::new()
            } else {
                format!(" · re-reads{cost}")
            }
        ));
        out["cold"] = json!(true);
        return out;
    }
    if expires.is_some_and(|n| n - now > 86400.) {
        expires = None;
    }
    let Some(expires) = expires else {
        if cache["warm"] != true {
            return out;
        }
        let ttl = string(&cache["ttl"]).trim();
        let ttl = super::render::text::one_line(ttl, 8);
        out["text"] = json!(format!(
            "CACHE {}",
            if ttl.is_empty() { "WARM" } else { &ttl }
        ));
        out["long"] = json!(format!(
            "WARM{}",
            if ttl.is_empty() {
                String::new()
            } else {
                format!(" · ttl {ttl}")
            }
        ));
        return out;
    };
    let left = expires - now;
    out["left"] = json!(left);
    out["ttl"] = json!(ttl_seconds(string(&cache["ttl"])).map(|n| n.max(left)));
    if left < 60. {
        out["text"] = json!("CACHE <1m");
        out["sev"] = json!("amber");
        out["long"] = json!("WARM · under 1m left");
    } else {
        out["text"] = json!(format!("CACHE {}", span(left)));
        out["sev"] = json!(if left >= 300. { "normal" } else { "amber" });
        out["long"] = json!(format!("WARM · {} left", span(left)));
    }
    out
}
pub fn reset_label(resets: f64, now: f64) -> String {
    let Some(when) = Local.timestamp_opt(resets as i64, 0).single() else {
        return String::new();
    };
    let today = Local
        .timestamp_opt(now as i64, 0)
        .single()
        .map(|v| v.date_naive());
    when.format(if Some(when.date_naive()) == today {
        "%H:%M"
    } else {
        "%a %H:%M"
    })
    .to_string()
}
pub fn pace_of(code: &str, resets: Option<f64>, now: f64) -> Option<f64> {
    let len = match code {
        "5H" => 18000.,
        "7D" => 604800.,
        _ => return None,
    };
    let left = resets? - now;
    (left > 0. && left <= len).then_some(1. - left / len)
}
pub fn quota_slot(code: &str, window: &Value, now: f64) -> Value {
    let used = percent_of(&window["used_percentage"]);
    let mut out = json!({
        "code": code,
        "text": format!("{code} --"),
        "sev": "normal",
        "used": null,
        "resets": null,
        "stale": false,
        "pace": null,
    });
    let Some(used) = used else {
        return out;
    };
    let resets = seconds_of(&window["resets_at"]);
    out["resets"] = json!(resets);
    if resets.is_some_and(|n| n <= now) {
        out["stale"] = json!(true);
        out["sev"] = json!("amber");
        return out;
    }
    out["used"] = json!(used);
    out["pace"] = json!(pace_of(code, resets, now));
    out["sev"] = json!(if used >= 90. {
        "red"
    } else if used > 70. {
        "amber"
    } else {
        "normal"
    });
    let mut text = format!("{code} {}%", round(used) as i64);
    if used > 70. {
        if let Some(at) = resets {
            text.push_str(&format!(" ↻{}", reset_label(at, now)));
        }
    }
    out["text"] = json!(text);
    out
}
pub fn compaction_cost(floor: f64, context: f64, summary: f64) -> f64 {
    floor * 2. + summary * 5. + context * 0.1
}
pub fn request_saving(floor: f64, context: f64) -> f64 {
    (context - floor) * 0.1
}
pub fn payback_requests(floor: f64, context: f64, summary: f64) -> Option<f64> {
    let saved = request_saving(floor, context);
    (saved > 0.).then(|| compaction_cost(floor, context, summary) / saved)
}
pub fn requests_per_turn(main: &Value, prompts: &Value) -> Option<f64> {
    let mut times: Vec<_> = array(main).iter().map(|r| num(&r[0])).collect();
    times.sort_by(f64::total_cmp);
    let mut prompts: Vec<_> = array(prompts).iter().map(num).collect();
    prompts.sort_by(f64::total_cmp);
    let counts: Vec<_> = prompts
        .windows(2)
        .map(|w| times.partition_point(|t| *t < w[1]) - times.partition_point(|t| *t < w[0]))
        .filter(|n| *n > 0)
        .collect();
    let counts = &counts[counts.len().saturating_sub(10)..];
    (counts.len() >= 2).then(|| counts.iter().sum::<usize>() as f64 / counts.len() as f64)
}
pub fn turn_ends(main: &Value, prompts: &Value) -> Vec<(f64, f64)> {
    let mut main: Vec<_> = array(main)
        .iter()
        .map(|r| (num(&r[0]), num(&r[1])))
        .collect();
    main.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    let mut prompts: Vec<_> = array(prompts).iter().map(num).collect();
    prompts.sort_by(f64::total_cmp);
    prompts
        .iter()
        .enumerate()
        .filter_map(|(i, t)| {
            let end = prompts.get(i + 1).copied().unwrap_or(f64::INFINITY);
            let low = main.partition_point(|r| r.0 < *t);
            let high = main.partition_point(|r| r.0 < end);
            (high > low).then(|| (*t, main[high - 1].1))
        })
        .collect()
}
pub fn payback_slot(
    floor: f64,
    measured: bool,
    tokens: f64,
    summary: f64,
    per_turn: Option<f64>,
) -> Value {
    if tokens < 180589. {
        return json!({
            "text": "",
            "sev": "normal",
            "forms": [],
        });
    }
    let requests = payback_requests(floor, tokens, summary);
    let turns = requests.map(|n| n / per_turn.unwrap_or(1.));
    if turns.is_none_or(|n| n > 15.) {
        let s = format!(
            "♻ >15 {}",
            if per_turn.is_some() {
                "prompts"
            } else {
                "calls"
            }
        );
        return json!({
            "text": s,
            "sev": "normal",
            "forms": [s],
        });
    }
    let requests = requests.unwrap();
    let calls = (requests + 0.5).floor().max(1.) as i64;
    let mut forms = vec![];
    if let Some(per) = per_turn {
        let n = (requests / per).ceil().max(1.) as i64;
        forms.push(format!(
            "♻ ~{n} {}",
            if n == 1 { "prompt" } else { "prompts" }
        ));
    }
    forms.push(format!(
        "♻ {}{calls} {}",
        if measured { "" } else { "~" },
        if calls == 1 { "call" } else { "calls" }
    ));
    json!({
        "text": forms[0],
        "forms": forms,
        "sev": if turns.unwrap() <= 1. {
            "amber"
        } else if turns.unwrap() <= 3. {
            "advisory"
        } else {
            "normal"
        },
    })
}
pub fn gauges(
    fields: &Value,
    at: Option<f64>,
    s: &Value,
    now: f64,
    limits: &Value,
    sts: &Value,
) -> Value {
    let (tokens, size) = ctx_now(fields, at, s);
    let sev = if tokens.is_none_or(|n| n < ctx_amber()) {
        "normal"
    } else if tokens.unwrap() < ctx_red() {
        "amber"
    } else {
        "red"
    };
    let (floor, measured) = floor_of(&s["compact_sizes"]);
    let summary = summary_of(&s["summary_sizes"]).0;
    let payback = tokens
        .map(|n| {
            payback_slot(
                floor,
                measured,
                n,
                summary,
                requests_per_turn(&s["main"], &s["prompts"]),
            )
        })
        .unwrap_or(json!({
            "text": "♻ --",
            "sev": "normal",
        }));
    let rates = if fields["rate_limits"]
        .as_object()
        .is_some_and(|v| !v.is_empty())
    {
        &fields["rate_limits"]
    } else {
        limits
    };
    let mut quotas = vec![
        quota_slot("5H", &rates["five_hour"], now),
        quota_slot("7D", &rates["seven_day"], now),
    ];
    quotas.sort_by(|a, b| {
        a["used"]
            .as_f64()
            .map(|v| -v)
            .unwrap_or(1.)
            .total_cmp(&b["used"].as_f64().map(|v| -v).unwrap_or(1.))
    });
    let ends = turn_ends(&s["main"], &s["prompts"]);
    let since = array(&s["compacts"]).iter().map(num).fold(0., f64::max);
    let vals: Vec<_> = ends
        .iter()
        .filter(|(t, _)| *t > since)
        .map(|(_, c)| *c)
        .collect();
    let diffs: Vec<_> = vals.windows(2).map(|w| w[1] - w[0]).collect();
    let trend = if vals.len() < 3 {
        None
    } else {
        median_of(&diffs[diffs.len().saturating_sub(5)..])
    };
    json!({
        "ctx": {
            "tokens": tokens,
            "size": size,
            "sev": sev,
        },
        "floor": {
            "tokens": floor,
            "measured": measured,
        },
        "trend": trend,
        "turns": ends
            .iter()
            .rev()
            .take(24)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|(_, c)| *c)
            .collect::<Vec<_>>(),
        "payback": payback,
        "cache": cache_info(&fields["prompt_cache"], now),
        "quotas": quotas,
        "sts": if sts.is_object() {
            sts.clone()
        } else {
            json!({
                "sev": null,
                "inop": [],
            })
        },
        "compacting": null,
    })
}
/// A usage file as `usage::measure` writes it.
#[cfg(test)]
fn kept_usage(p: &Paths, sid: &str, at: f64, usage: Value) {
    let mut body = usage;
    body["at"] = json!(at);
    let f = p.kept_file(super::usage::FOLDER, sid).unwrap();
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(f, body.to_string()).unwrap();
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_the_pace_is_the_share_of_the_window_gone() {
        assert_eq!(pace_of("5H", Some(10000. + 3600.), 10000.), Some(0.8));
        assert_eq!(pace_of("5H", Some(10000. - 1.), 10000.), None);
    }
    #[test]
    fn test_the_cache_keeps_its_clock_for_the_tape() {
        let c = cache_info(
            &json!({
                "warm": true,
                "ttl": "1h",
                "expires_at": 11800,
            }),
            10000.,
        );
        assert_eq!(c["left"], 1800.);
        assert_eq!(c["ttl"], 3600.);
        assert_eq!(c["text"], "CACHE 30m");
    }
    #[test]
    fn test_the_turns_are_each_turns_last_context() {
        assert_eq!(
            turn_ends(
                &json!([[105, 10], [106, 20], [205, 30], [405, 40]]),
                &json!([100, 200, 300, 400])
            ),
            vec![(100., 20.), (200., 30.), (400., 40.)]
        );
    }
    #[test]
    fn test_a_reset_rolls_over_to_unknown() {
        let q = quota_slot(
            "5H",
            &json!({
                "used_percentage": 80,
                "resets_at": 9999,
            }),
            10000.,
        );
        assert_eq!(q["text"], "5H --");
        assert_eq!(q["stale"], true);
    }
    #[test]
    fn test_quota_rounds_ties_even() {
        assert_eq!(
            quota_slot(
                "5H",
                &json!({
                    "used_percentage": 12.5
                }),
                10000.
            )["text"],
            "5H 12%"
        );
    }
    #[test]
    fn test_newer_transcript_wins_over_status() {
        let s = json!({
            "main": [[1006, 120000]],
        });
        assert_eq!(
            ctx_now(
                &json!({
                    "context_window": {
                        "total_input_tokens": 50000
                    }
                }),
                Some(1000.),
                &s
            )
            .0,
            Some(120000.)
        );
    }
}

#[cfg(test)]
mod parity_tests {
    use super::super::render::text::{cockpit, fit_pieces, plain};
    use super::*;
    fn g(tokens: f64) -> Value {
        gauges(
            &json!({
                "context_window": {
                    "total_input_tokens": tokens,
                    "context_window_size": 1000000,
                },
            }),
            Some(10000.),
            &Value::Null,
            10000.,
            &Value::Null,
            &Value::Null,
        )
    }
    #[test]
    fn test_requests_to_break_even_from_this_sessions_own_median_post_size() {
        let r = payback_requests(64971., 200000., 21700.).unwrap();
        assert!((r - 19.14).abs() < 0.005);
        assert!((r / 17. - 1.13).abs() < 0.005);
    }
    #[test]
    fn test_a_bigger_context_pays_back_sooner() {
        assert!(
            payback_requests(64971., 300000., 21700.) < payback_requests(64971., 220000., 21700.)
        );
    }
    #[test]
    fn test_nothing_at_or_under_the_floor() {
        assert_eq!(payback_requests(64971., 60000., 21700.), None);
        assert_eq!(payback_requests(64971., 64971., 21700.), None);
        assert_eq!(median_last(&[], 65000.), (65000., false));
    }
    #[test]
    fn test_an_even_number_of_sizes_takes_the_middle_pair() {
        assert_eq!(median_last(&[64971., 64115.], 65000.), (64543., true));
    }
    #[test]
    fn test_a_reading_at_or_under_the_floor_never_pays_back_and_says_so() {
        assert_eq!(g(60000.)["payback"]["text"], "");
        let g = gauges(
            &Value::Null,
            None,
            &Value::Null,
            10000.,
            &Value::Null,
            &Value::Null,
        );
        assert_eq!(g["payback"]["text"], "♻ --");
    }
    #[test]
    fn test_no_payback_under_the_band_csr_offers_in() {
        for n in [95000., 180588.] {
            assert_eq!(g(n)["payback"]["text"], "");
        }
        assert!(string(&g(180589.)["payback"]["text"]).starts_with('♻'));
    }
    #[test]
    fn test_the_payback_ink_follows_the_turns_to_repay() {
        assert_eq!(
            payback_slot(65000., true, 300000., 21700., Some(17.))["sev"],
            "amber"
        );
        assert_eq!(
            payback_slot(65000., true, 200000., 21700., Some(17.))["sev"],
            "advisory"
        );
    }
    #[test]
    fn test_the_context_row_reads_the_context_against_its_window_and_the_payback() {
        assert_eq!(plain(&cockpit(&g(87724.), 49)[0]), " CTX 88k/1M");
        assert_eq!(
            plain(&cockpit(&g(250000.), 49)[0]),
            " CTX 250k/1M  ♻ ~14 calls"
        );
    }
    #[test]
    fn test_the_context_lights_amber_at_100k_and_red_at_400k_and_its_tape_pegs() {
        assert_eq!(g(150000.)["ctx"]["sev"], "amber");
        assert_eq!(g(432000.)["ctx"]["sev"], "red");
    }
    #[test]
    fn test_the_floor_is_the_median_of_the_last_five_posts() {
        assert_eq!(median_last(&[100., 201.], 65000.), (151., true));
        assert_eq!(
            median_last(&[1., 100., 200., 300., 400., 500., 600.], 65000.),
            (400., true)
        );
    }
    #[test]
    fn test_the_newest_reply_beats_a_kept_status_line_older_than_it() {
        let fields = json!({
            "context_window": {
                "total_input_tokens": 80000,
                "context_window_size": 1000000,
            },
        });
        let s = json!({
            "main": [[9990, 91000]],
        });
        assert_eq!(
            ctx_now(&fields, Some(9940.), &s),
            (Some(91000.), Some(1000000.))
        );
        assert_eq!(
            ctx_now(&fields, Some(9988.), &s),
            (Some(80000.), Some(1000000.))
        );
        assert_eq!(
            ctx_now(
                &json!({
                    "context_window": {
                        "current_usage": {
                            "input_tokens": 2,
                            "cache_read_input_tokens": 30,
                            "cache_creation_input_tokens": 8
                        }
                    }
                }),
                None,
                &Value::Null
            ),
            (Some(40.), None)
        );
    }
    #[test]
    fn test_the_riskier_window_comes_first() {
        for (five, seven, first) in [(8, 75, "7D"), (92, 75, "5H"), (30, 30, "5H")] {
            let g = gauges(
                &json!({
                    "rate_limits": {
                        "five_hour": {
                            "used_percentage": five,
                        },
                        "seven_day": {
                            "used_percentage": seven,
                        },
                    },
                }),
                None,
                &Value::Null,
                10000.,
                &Value::Null,
                &Value::Null,
            );
            assert_eq!(g["quotas"][0]["code"], first);
        }
    }
    #[test]
    fn test_a_value_that_went_invalid_is_dashed_in_amber_and_one_never_seen_in_the_normal_ink() {
        let stale = quota_slot(
            "5H",
            &json!({
                "used_percentage": 95,
                "resets_at": 9940,
            }),
            10000.,
        );
        assert_eq!(stale["text"], "5H --");
        assert_eq!(stale["sev"], "amber");
        assert_eq!(quota_slot("5H", &Value::Null, 10000.)["sev"], "normal");
    }
    #[test]
    fn test_the_account_limits_come_from_another_session_within_a_day() {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::new(d.path().into());
        for (id, at, used) in [("a", 9990, 12), ("b", 9995, 34), ("c", 0, 99)] {
            kept_usage(
                &p,
                id,
                at as f64,
                json!({
                    "rateLimits": [{ "kind": "five_hour", "percentUsed": used }],
                }),
            );
        }
        assert_eq!(
            account_limits(&p, 10000.)["five_hour"]["used_percentage"],
            34.
        );
        assert_eq!(account_limits(&p, 100000.), json!({}));
    }
    #[test]
    fn test_two_rows_at_every_width_and_sts_always_survives() {
        let mut g = g(432000.);
        g["sts"] = json!({
            "sev": "amber",
        });
        for width in 24..81 {
            let rows = cockpit(&g, width);
            assert_eq!(rows.len(), 2);
            for row in &rows {
                assert!(plain(row).chars().count() <= width);
            }
            assert!(plain(&rows[1]).ends_with("STS"));
        }
    }
    #[test]
    fn test_facts_drop_their_rightmost_pieces_to_fit() {
        let s = "12 prompts · 3h20m · compact repays in ~6t · 13 compacts";
        assert_eq!(fit_pieces(s, 20), "12 prompts · 3h20m");
        assert_eq!(fit_pieces(s, 6), "12 pr…");
    }
    #[test]
    fn test_numbers_no_clock_can_show_never_raise() {
        for v in [json!(true), json!(-5), json!(1e20), Value::Null, json!("x")] {
            assert_eq!(seconds_of(&v), None);
        }
        assert_eq!(
            cache_info(
                &json!({
                    "warm": true,
                    "expires_at": 1e20
                }),
                10000.
            )["text"],
            "CACHE WARM"
        );
        assert_eq!(
            quota_slot(
                "5H",
                &json!({
                    "used_percentage": "lots"
                }),
                10000.
            )["text"],
            "5H --"
        );
    }
    #[test]
    fn test_fields_come_back_from_the_kept_file_and_not_when_a_day_old() {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::new(d.path().into());
        kept_usage(
            &p,
            "s",
            1000.,
            json!({ "context": { "tokens": 40000, "window": 1000000 } }),
        );
        let kept = kept_status(&p, "s", 1100.);
        assert_eq!(kept["at"], 1000.);
        assert_eq!(
            kept["fields"]["context_window"]["total_input_tokens"],
            40000.
        );
        assert_eq!(
            kept["fields"]["context_window"]["context_window_size"],
            1000000.
        );
        assert!(kept_status(&p, "s", 87400.).is_null());
    }
    #[test]
    fn test_a_daemon_lock_with_an_absurd_number_is_a_daemon_that_is_down() {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::new(d.path().into());
        std::fs::create_dir_all(p.db.parent().unwrap()).unwrap();
        std::fs::write(
            p.home.join(".claude-self-reflect/daemon.lock"),
            "9".repeat(1000),
        )
        .unwrap();
        assert_eq!(csr_status(&p)["inop"][0][0], "DAEMON");
    }
}

#[cfg(test)]
mod status_cache_tests {
    use super::*;
    #[test]
    fn test_sts_is_read_once_a_minute() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().into());
        std::fs::create_dir_all(paths.db.parent().unwrap()).unwrap();
        assert_eq!(csr_status_at(&paths, 1000.)["daemon"]["up"], false);
        std::fs::write(
            paths.home.join(".claude-self-reflect/daemon.lock"),
            std::process::id().to_string(),
        )
        .unwrap();
        assert_eq!(csr_status_at(&paths, 1001.)["daemon"]["up"], false);
        assert_eq!(csr_status_at(&paths, 1061.)["daemon"]["up"], true);
    }
    #[test]
    fn test_a_reset_tomorrow_names_its_day_even_under_a_day_away() {
        let late = Local
            .with_ymd_and_hms(2027, 1, 15, 23, 0, 0)
            .single()
            .unwrap()
            .timestamp() as f64;
        assert_eq!(reset_label(late + 8. * 3600., late), "Sat 07:00");
        let morning = Local
            .with_ymd_and_hms(2027, 1, 15, 9, 0, 0)
            .single()
            .unwrap()
            .timestamp() as f64;
        assert_eq!(reset_label(morning + 3600., morning), "10:00");
    }
    #[test]
    fn test_ttl_is_a_decimal_clock_not_a_general_float_literal() {
        for text in ["1e-1h", "+1h", ".5h", "1.h", "0m", "2d", "NaNh"] {
            assert_eq!(ttl_seconds(text), None, "{text}");
        }
        assert_eq!(ttl_seconds(" 2h "), Some(7200.));
    }
}

#[cfg(test)]
mod quota_cache_tests {
    use super::*;
    #[test]
    fn test_an_empty_newer_status_does_not_hide_account_quotas() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().into());
        let usage = |used: f64| {
            json!({
                "rateLimits": [{ "kind": "five_hour", "percentUsed": used }],
            })
        };
        kept_usage(&paths, "a", 9990., usage(34.));
        kept_usage(&paths, "b", 9995., json!({ "rateLimits": [] }));
        assert_eq!(
            account_limits(&paths, 10000.)["five_hour"]["used_percentage"],
            34.
        );
    }
    #[test]
    fn test_sts_names_what_csr_runs_without_and_never_reads_out_another_variable() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().into());
        std::fs::create_dir_all(paths.home.join(".claude")).unwrap();
        std::fs::create_dir_all(paths.db.parent().unwrap()).unwrap();
        std::fs::write(
            paths.home.join(".claude-self-reflect/daemon.lock"),
            std::process::id().to_string(),
        )
        .unwrap();
        std::fs::write(
            paths.home.join(".claude/settings.json"),
            json!({
                "env": {
                    "CSR_NO_AI_NARRATIVES": "true",
                    "CSR_NO_DREAMING": "false",
                    "CSR_NO_RATIFICATION": "1",
                    "SECRET": "shh",
                },
            })
            .to_string(),
        )
        .unwrap();
        let got = csr_status_at(&paths, 1000.);
        assert_eq!(got["sev"], "advisory");
        assert_eq!(
            got["inop"],
            json!([[
                "AI_NARRATIVES",
                "INHIBITED · CSR_NO_AI_NARRATIVES=true (hooks)",
                "advisory"
            ]])
        );
        assert_eq!(got["daemon"]["up"], true);
        assert!(!got.to_string().contains("shh"));
        assert!(!got.to_string().contains("SECRET"));
    }
    #[test]
    fn test_a_plist_that_is_not_xml_is_skipped_and_the_others_still_read() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().into());
        let folder = paths.home.join("Library/LaunchAgents");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join("com.x.csr-engine.broken.plist"),
            "<plist><dict><key>EnvironmentVariables",
        )
        .unwrap();
        std::fs::write(
            folder.join("com.x.csr-engine.binary.plist"),
            b"bplist00garbage",
        )
        .unwrap();
        std::fs::write(
            folder.join("com.x.csr-engine.daemon.plist"),
            "<plist><dict><key>EnvironmentVariables</key><dict><key>CSR_NO_RE\
                CAP</key><string>1</string></dict></dict></plist>",
        )
        .unwrap();
        std::fs::create_dir_all(paths.db.parent().unwrap()).unwrap();
        std::fs::write(
            paths.home.join(".claude-self-reflect/daemon.lock"),
            std::process::id().to_string(),
        )
        .unwrap();
        assert_eq!(csr_status_at(&paths, 1000.)["inop"][0][0], "RECAP");
    }
}

#[cfg(test)]
mod derived_rows_tests {
    use super::super::render::text::{cockpit, plain, slot};
    use super::*;
    #[test]
    fn test_the_floor_is_the_median_of_the_last_five_posts() {
        assert_eq!(floor_of(&json!([null, 100, 201])), (151., true));
        assert_eq!(
            floor_of(&json!([[null, 100, null], [null, 201, null]])),
            (151., true)
        );
        assert_eq!(summary_of(&json!([null, 19000, 0, 24000])), (21500., true));
        assert_eq!(median_last(&[100.5], 0.), (100.5, true));
    }
    #[test]
    fn test_the_trend_is_the_median_of_the_turn_to_turn_steps_since_the_compaction() {
        let main = json!([
            [205, 98000],
            [210, 100000],
            [305, 110000],
            [310, 112000],
            [405, 118000],
            [410, 120000],
            [505, 133000],
            [510, 135000],
        ]);
        let s = json!({
            "main": main,
            "prompts": [200, 300, 400, 500],
            "compacts": [50],
        });
        let g = gauges(&Value::Null, None, &s, 10000., &Value::Null, &Value::Null);
        assert_eq!(g["trend"], 12000.);
        assert!(plain(&cockpit(&g, 60)[0]).contains("▲12k/t"));
        let down = json!({
            "main": [[105, 130000], [205, 120000], [305, 108000]],
            "prompts": [100, 200, 300],
        });
        assert_eq!(
            gauges(
                &Value::Null,
                None,
                &down,
                10000.,
                &Value::Null,
                &Value::Null
            )["trend"],
            -11000.
        );
    }
    #[test]
    fn test_sts_lights_row_two_in_its_severity_ink() {
        for sev in ["advisory", "amber"] {
            let g = gauges(
                &Value::Null,
                None,
                &Value::Null,
                10000.,
                &Value::Null,
                &json!({
                    "sev": sev,
                }),
            );
            let row = cockpit(&g, 49).remove(1);
            assert!(row.ends_with(&slot("STS", sev)));
        }
        assert!(!plain(
            &cockpit(
                &gauges(
                    &Value::Null,
                    None,
                    &Value::Null,
                    10000.,
                    &Value::Null,
                    &Value::Null
                ),
                49
            )[1]
        )
        .contains("STS"));
    }
    #[test]
    fn test_a_window_ahead_of_pace_carries_a_mark_after_its_percentage() {
        use super::super::render::text::paced;
        let q = quota_slot(
            "5H",
            &json!({
                "used_percentage": 95,
                "resets_at": 10000. + 7200.,
            }),
            10000.,
        );
        assert!(paced(&q).starts_with("5H 95%» ↻"));
        let q = quota_slot(
            "5H",
            &json!({
                "used_percentage": 8,
                "resets_at": 10000. + 7200.,
            }),
            10000.,
        );
        assert_eq!(paced(&q), "5H 8%");
    }
}

pub fn clock_text(seconds: f64) -> String {
    let s = seconds as i64;
    format!("{}:{:02}", s.div_euclid(60), s.rem_euclid(60))
}
pub fn compaction_bar(elapsed: f64, high: f64, width: usize) -> String {
    let cells = if high != 0. {
        (elapsed / high).clamp(0., 1.) * width as f64
    } else {
        0.
    };
    let full = cells as usize;
    if full >= width {
        return "█".repeat(width);
    }
    let fractions = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
    let tail = fractions[((cells - full as f64) * 8.) as usize];
    let mut out = "█".repeat(full);
    out.push_str(tail);
    out.push_str(&"░".repeat(width - full - usize::from(!tail.is_empty())));
    out
}
pub fn compacting_slot(info: &Value, bar: bool, range: bool) -> (String, &'static str) {
    let usual = &info["usual"];
    let elapsed = num(&info["elapsed"]);
    let mut text = format!(
        "{} {}",
        if info["trigger"] == "auto" {
            "♻A"
        } else {
            "♻"
        },
        clock_text(elapsed)
    );
    if usual.is_object() && bar {
        text.push_str(&format!(
            " {}",
            compaction_bar(elapsed, num(&usual["high"]), 8)
        ));
        if range {
            text.push_str(&format!(
                " {}–{}",
                clock_text(num(&usual["low"])),
                clock_text(num(&usual["high"]))
            ));
        }
    }
    (
        text,
        if usual.is_object() && elapsed > num(&usual["high"]) {
            "amber"
        } else {
            "advisory"
        },
    )
}
pub fn gauges_compacting(
    fields: &Value,
    at: Option<f64>,
    s: &Value,
    now: f64,
    limits: &Value,
    sts: &Value,
    compacting: &Value,
) -> Value {
    let mut g = gauges(fields, at, s, now, limits, sts);
    if compacting.is_object() {
        let (text, sev) = compacting_slot(compacting, true, true);
        g["payback"] = json!({
            "text": text,
            "sev": sev,
        });
        g["compacting"] = compacting.clone();
    }
    g
}
#[cfg(test)]
mod timer_tests {
    use super::super::render::text::{cockpit, plain};
    use super::*;
    #[test]
    fn test_while_a_compaction_runs_its_timer_takes_the_paybacks_slot() {
        let info = json!({
            "elapsed": 72,
            "trigger": "manual",
            "usual": {
                "low": 50,
                "median": 75,
                "high": 100,
                "n": 9,
            },
            "facts": ["CSR keeps 12 chunks of this session"],
        });
        let fields = json!({
            "context_window": {
                "total_input_tokens": 300000,
                "context_window_size": 1000000,
            },
            "prompt_cache": {
                "warm": true,
                "expires_at": 13000,
            },
        });
        let g = gauges_compacting(
            &fields,
            Some(10000.),
            &Value::Null,
            10000.,
            &Value::Null,
            &Value::Null,
            &info,
        );
        assert_eq!(
            plain(&cockpit(&g, 49)[0]),
            " CTX 300k/1M  ♻ 1:12 █████▊░░ 0:50–1:40"
        );
        assert!(plain(&cockpit(&g, 49)[1]).ends_with("CACHE 50m"));
        let g = gauges_compacting(
            &Value::Null,
            None,
            &Value::Null,
            10000.,
            &Value::Null,
            &Value::Null,
            &info,
        );
        for (width, want) in [
            (34, " CTX --  ♻ 1:12 █████▊░░ 0:50–1:40"),
            (33, " CTX --  ♻ 1:12 █████▊░░"),
            (23, " CTX --  ♻ 1:12"),
        ] {
            assert_eq!(plain(&cockpit(&g, width)[0]), want);
        }
        for width in 10..60 {
            for row in cockpit(&g, width) {
                assert!(plain(&row).chars().count() <= width);
            }
        }
    }
    #[test]
    fn test_old_compaction_markers_are_ignored_and_milliseconds_are_seconds() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().into());
        let marker = paths.kept_file("compacting", "s").unwrap();
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(
            &marker,
            json!({
                "at": 1800000000000_u64,
                "trigger": "auto",
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            compacting_info(&paths, "s", 1800000005., false)["elapsed"],
            5.
        );
        assert!(compacting_info(&paths, "s", 1800000601., false).is_null());
        assert!(!paths.keep.join("compaction-times.json").exists());
    }
}

#[cfg(test)]
mod usage_tests {
    use super::super::render::text::{cockpit, plain};
    use super::*;

    const T0: f64 = 1792320000.;

    /// What `session.measure` carried in Claude Code 2.1.303 (WP0).
    fn wp0() -> Value {
        json!({
            "context": { "percent": 3, "tokens": 34581, "window": 1000000 },
            "cost": { "usd": 0.00381693 },
            "rateLimits": [
                { "kind": "five_hour", "percentUsed": 11, "resetsAt": "2026-10-19T19:00:00.000Z" },
                { "kind": "seven_day", "percentUsed": 25, "resetsAt": "2026-10-25T14:00:00.000Z" }
            ]
        })
    }
    fn gauges_of(fields: &Value, now: f64) -> Value {
        gauges(
            fields,
            Some(now),
            &Value::Null,
            now,
            &Value::Null,
            &Value::Null,
        )
    }
    fn rows(g: &Value) -> Vec<String> {
        cockpit(g, 49).iter().map(|r| plain(r)).collect()
    }

    #[test]
    fn test_the_masthead_from_the_wp0_body_reads_as_the_status_line_figures_did() {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::new(d.path().into());
        let now = 1792400000.;
        kept_usage(&p, "s", now - 10., wp0());
        let kept = kept_status(&p, "s", now);
        let old = json!({
            "context_window": {
                "total_input_tokens": 34581,
                "context_window_size": 1000000,
            },
            "rate_limits": {
                "five_hour": { "used_percentage": 11, "resets_at": 1792436400. },
                "seven_day": { "used_percentage": 25, "resets_at": 1792936800. },
            },
        });
        let new = gauges_of(&kept["fields"], now);
        let before = gauges_of(&old, now);
        assert_eq!(new, before);
        assert_eq!(rows(&new), rows(&before));
        assert_eq!(
            rows(&new),
            vec![" CTX 35k/1M", " 7D 25%»  5H 11%  CACHE --"]
        );
    }
    #[test]
    fn test_resets_at_is_an_iso_time_and_comes_out_as_epoch_seconds() {
        let fields = usage_fields(&wp0());
        assert_eq!(fields["rate_limits"]["five_hour"]["resets_at"], 1792436400.);
        assert_eq!(fields["rate_limits"]["seven_day"]["resets_at"], 1792936800.);
        let bare = usage_fields(&json!({
            "rateLimits": [
                { "kind": "five_hour", "percentUsed": 80, "resetsAt": "not a time" },
            ],
        }));
        assert_eq!(
            bare["rate_limits"]["five_hour"],
            json!({ "used_percentage": 80. })
        );
        let q = quota_slot("5H", &fields["rate_limits"]["five_hour"], 1792436000.);
        assert_eq!(q["resets"], 1792436400.);
    }
    #[test]
    fn test_a_window_that_is_over_70_percent_names_its_reset() {
        let fields = usage_fields(&json!({
            "rateLimits": [
                { "kind": "five_hour", "percentUsed": 80, "resetsAt": "2026-10-19T19:00:00Z" },
            ],
        }));
        let q = quota_slot("5H", &fields["rate_limits"]["five_hour"], 1792436000.);
        assert!(string(&q["text"]).starts_with("5H 80% ↻"));
    }
    #[test]
    fn test_a_kind_the_masthead_has_no_cell_for_is_kept_apart_and_never_shown() {
        let fields = usage_fields(&json!({
            "rateLimits": [
                { "kind": "spend_limit", "percentUsed": 120 },
                { "kind": "five_hour", "percentUsed": 40 },
            ],
        }));
        assert_eq!(
            fields["rate_limits"]["spend_limit"]["used_percentage"],
            120.
        );
        let g = gauges_of(&fields, 10000.);
        let codes: Vec<_> = array(&g["quotas"])
            .iter()
            .map(|q| string(&q["code"]))
            .collect();
        assert_eq!(codes, ["5H", "7D"]);
        assert_eq!(g["quotas"][0]["used"], 40.);
        assert_eq!(g["quotas"][1]["text"], "7D --");
    }
    #[test]
    fn test_an_empty_rate_limits_leaves_the_cells_to_the_account_figures() {
        let fields = usage_fields(&json!({
            "context": { "tokens": 50000, "window": 200000 },
            "rateLimits": [],
        }));
        assert_eq!(fields["rate_limits"], json!({}));
        let account = json!({ "five_hour": { "used_percentage": 61. } });
        let g = gauges(
            &fields,
            Some(9990.),
            &Value::Null,
            10000.,
            &account,
            &Value::Null,
        );
        assert_eq!(g["quotas"][0]["text"], "5H 61%");
        assert_eq!(g["ctx"]["tokens"], 50000.);
        let off = gauges(
            &fields,
            Some(9990.),
            &Value::Null,
            10000.,
            &json!({}),
            &Value::Null,
        );
        assert_eq!(off["quotas"][0]["text"], "5H --");
        assert_eq!(off["quotas"][1]["text"], "7D --");
    }
    #[test]
    fn test_a_context_with_no_tokens_yet_is_the_percent_of_its_window_or_nothing() {
        let by_percent = usage_fields(&json!({ "context": { "window": 200000, "percent": 5 } }));
        assert_eq!(by_percent["context_window"]["total_input_tokens"], 10000.);
        let none = usage_fields(&json!({ "context": { "window": 200000 } }));
        assert!(none["context_window"]["total_input_tokens"].is_null());
        assert_eq!(none["context_window"]["context_window_size"], 200000.);
        assert_eq!(usage_fields(&json!({})), json!({ "rate_limits": {} }));
    }
    #[test]
    fn test_the_newest_reply_beats_a_usage_file_older_than_it() {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::new(d.path().into());
        kept_usage(
            &p,
            "s",
            9940.,
            json!({ "context": { "tokens": 80000, "window": 1000000 } }),
        );
        let kept = kept_status(&p, "s", 10000.);
        let s = json!({ "main": [[9990, 91000]] });
        assert_eq!(
            ctx_now(&kept["fields"], kept["at"].as_f64(), &s),
            (Some(91000.), Some(1000000.))
        );
        kept_usage(
            &p,
            "s",
            9988.,
            json!({ "context": { "tokens": 80000, "window": 1000000 } }),
        );
        let kept = kept_status(&p, "s", 10000.);
        assert_eq!(
            ctx_now(&kept["fields"], kept["at"].as_f64(), &s),
            (Some(80000.), Some(1000000.))
        );
    }

    fn stamp(t: f64) -> String {
        chrono::DateTime::from_timestamp(t as i64, 0)
            .unwrap()
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    }
    fn reply(t: f64, usage: Value, side: bool) -> String {
        json!({
            "type": "assistant",
            "timestamp": stamp(t),
            "isSidechain": side,
            "message": { "id": format!("m{t}"), "usage": usage },
        })
        .to_string()
    }
    fn transcript(p: &Paths, sid: &str, lines: &[String]) {
        let f = p.projects.join("proj").join(format!("{sid}.jsonl"));
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, lines.join("\n") + "\n").unwrap();
    }
    fn cache_at(p: &Paths, sid: &str, now: f64) -> Value {
        let kept = kept_status(p, sid, now);
        cache_info(&kept["fields"]["prompt_cache"], now)
    }
    fn five_minute() -> Value {
        json!({
            "input_tokens": 4,
            "cache_read_input_tokens": 30000,
            "cache_creation_input_tokens": 4000,
            "cache_creation": {
                "ephemeral_5m_input_tokens": 4000,
                "ephemeral_1h_input_tokens": 0,
            },
        })
    }
    fn one_hour() -> Value {
        json!({
            "input_tokens": 4,
            "cache_read_input_tokens": 30000,
            "cache_creation_input_tokens": 4000,
            "cache_creation": {
                "ephemeral_5m_input_tokens": 0,
                "ephemeral_1h_input_tokens": 4000,
            },
        })
    }
    #[test]
    fn test_a_five_minute_cache_is_warm_at_4_minutes_and_cold_at_6() {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::new(d.path().into());
        transcript(&p, "s", &[reply(T0, five_minute(), false)]);
        let warm = cache_at(&p, "s", T0 + 240.);
        assert_eq!(warm["text"], "CACHE 1m");
        assert_eq!(warm["sev"], "amber");
        assert_eq!(warm["cold"], false);
        let cold = cache_at(&p, "s", T0 + 360.);
        assert_eq!(cold["text"], "CACHE COLD ~34k");
        assert_eq!(cold["cold"], true);
    }
    #[test]
    fn test_an_hour_cache_is_warm_at_50_minutes_and_cold_at_61() {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::new(d.path().into());
        transcript(&p, "s", &[reply(T0, one_hour(), false)]);
        let warm = cache_at(&p, "s", T0 + 3000.);
        assert_eq!(warm["text"], "CACHE 10m");
        assert_eq!(warm["sev"], "normal");
        assert_eq!(warm["ttl"], 3600.);
        let cold = cache_at(&p, "s", T0 + 3660.);
        assert_eq!(cold["text"], "CACHE COLD ~34k");
    }
    #[test]
    fn test_no_reply_in_the_transcript_is_a_dashed_cache() {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::new(d.path().into());
        assert_eq!(cache_at(&p, "none", T0)["text"], "CACHE --");
        transcript(
            &p,
            "bare",
            &[json!({ "type": "user", "timestamp": stamp(T0) }).to_string()],
        );
        assert_eq!(cache_at(&p, "bare", T0 + 10.)["text"], "CACHE --");
    }
    #[test]
    fn test_the_cache_reads_the_last_main_thread_reply_and_not_a_side_agents() {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::new(d.path().into());
        let empty = json!({
            "type": "assistant",
            "timestamp": stamp(T0 + 250.),
            "message": { "usage": { "input_tokens": 0, "output_tokens": 0 } },
        });
        transcript(
            &p,
            "s",
            &[
                reply(T0 - 4000., one_hour(), false),
                reply(T0, five_minute(), false),
                reply(T0 + 200., one_hour(), true),
                empty.to_string(),
            ],
        );
        let kept = kept_status(&p, "s", T0 + 100.);
        assert_eq!(kept["fields"]["prompt_cache"]["ttl"], "5m");
        assert_eq!(kept["fields"]["prompt_cache"]["expires_at"], T0 + 300.);
    }
    #[test]
    fn test_the_recache_cost_is_the_usage_files_context_else_the_replys_input() {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::new(d.path().into());
        transcript(&p, "s", &[reply(T0, five_minute(), false)]);
        let bare = kept_status(&p, "s", T0 + 400.);
        let cost = &bare["fields"]["prompt_cache"]["recache_tokens_if_cold"];
        assert_eq!(*cost, 34004.);
        assert!(bare["at"].is_null());
        kept_usage(
            &p,
            "s",
            T0 + 390.,
            json!({ "context": { "tokens": 120000, "window": 1000000 } }),
        );
        let kept = kept_status(&p, "s", T0 + 400.);
        let cache = &kept["fields"]["prompt_cache"];
        assert_eq!(cache["recache_tokens_if_cold"], 120000.);
        assert_eq!(cache_info(cache, T0 + 400.)["text"], "CACHE COLD ~120k");
    }
    #[test]
    fn test_a_long_tool_result_after_the_last_reply_does_not_hide_it() {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::new(d.path().into());
        let big = json!({
            "type": "user",
            "timestamp": stamp(T0 + 1.),
            "message": { "content": "x".repeat(600 * 1024) },
        });
        transcript(&p, "s", &[reply(T0, one_hour(), false), big.to_string()]);
        let kept = kept_status(&p, "s", T0 + 60.);
        assert_eq!(kept["fields"]["prompt_cache"]["expires_at"], T0 + 3600.);
    }
    #[test]
    fn test_nothing_reads_or_writes_a_retired_status_line_folder() {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::new(d.path().into());
        let old = p.keep.join(concat!("status", "line"));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(
            old.join("s.json"),
            json!({
                "at": 9990,
                "fields": {
                    "context_window": { "total_input_tokens": 1 },
                    "rate_limits": { "five_hour": { "used_percentage": 9 } },
                },
            })
            .to_string(),
        )
        .unwrap();
        assert!(kept_status(&p, "s", 10000.).is_null());
        assert_eq!(account_limits(&p, 10000.), json!({}));
        let mut names: Vec<_> = std::fs::read_dir(&p.keep).unwrap().collect();
        assert_eq!(names.len(), 1);
        assert!(names.pop().unwrap().unwrap().path().ends_with(old));
    }
}
