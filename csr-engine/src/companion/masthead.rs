use super::{
    array, num,
    paths::{epoch, folders, read_json, Paths},
    pyfmt::{round, short},
    string,
};
use chrono::{Local, TimeZone};
use serde_json::{json, Value};

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
pub fn threshold(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|n| n.is_finite())
        .unwrap_or(default)
        .max(1.)
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
pub fn kept_status(paths: &Paths, sid: &str, now: f64) -> Value {
    let v = paths
        .kept_file("statusline", sid)
        .map(|p| read_json(&p))
        .unwrap_or(Value::Null);
    if v["fields"].is_object() && now - num(&v["at"]) < 86400. {
        v
    } else {
        Value::Null
    }
}
pub fn account_limits(paths: &Paths, now: f64) -> Value {
    type Hit = (std::time::SystemTime, u64, f64, Value);
    static CACHE: std::sync::LazyLock<
        std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, Hit>>,
    > = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let Ok(mut cache) = CACHE.lock() else {
        return json!({});
    };
    let mut best: Option<(f64, Value)> = None;
    for path in folders(&paths.keep.join("statusline")) {
        if path.extension().is_none_or(|s| s != "json") {
            continue;
        }
        let Ok(stat) = std::fs::metadata(&path) else {
            continue;
        };
        let Ok(modified) = stat.modified() else {
            continue;
        };
        if cache
            .get(&path)
            .is_none_or(|hit| hit.0 != modified || hit.1 != stat.len())
        {
            let kept = read_json(&path);
            let limits = &kept["fields"]["rate_limits"];
            cache.insert(
                path.clone(),
                (
                    modified,
                    stat.len(),
                    num(&kept["at"]),
                    if limits.is_object() {
                        limits.clone()
                    } else {
                        Value::Null
                    },
                ),
            );
        }
        let hit = &cache[&path];
        if hit.3.as_object().is_some_and(|v| !v.is_empty())
            && now - hit.2 < 86400.
            && best.as_ref().is_none_or(|(at, _)| hit.2 > *at)
        {
            best = Some((hit.2, hit.3.clone()));
        }
    }
    best.map(|(_, v)| v).unwrap_or(json!({}))
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
pub fn csr_status(paths: &Paths) -> Value {
    csr_status_at(paths, super::paths::now())
}
pub fn csr_status_at(paths: &Paths, now: f64) -> Value {
    type Cache = std::collections::HashMap<std::path::PathBuf, (f64, Value)>;
    static CACHE: std::sync::LazyLock<std::sync::Mutex<Cache>> =
        std::sync::LazyLock::new(|| std::sync::Mutex::new(Cache::new()));
    if let Ok(cache) = CACHE.lock() {
        if let Some((at, value)) = cache.get(&paths.home) {
            if now >= *at && now - *at < 60. {
                return value.clone();
            }
        }
    }

    let lock = paths.home.join(".claude-self-reflect/daemon.lock");
    let pid = std::fs::read_to_string(lock)
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
        .filter(|p| *p > 1);
    let up=pid.is_some_and(|pid|unsafe {
        libc::kill(pid,
        0)
    }==0||std::io::Error::last_os_error().raw_os_error()==Some(libc::EPERM));
    let mut inop = vec![];
    if !up {
        inop.push(json!([
            "DAEMON",
            "DOWN · no live pid in daemon.lock",
            "amber",
        ]));
    }
    let mut switches = vec![];
    let re =
        regex::Regex::new(r"<key>(CSR_NO_[A-Z0-9_]+)</key>\s*<string>([^<]*)</string>").unwrap();
    for p in folders(&paths.home.join("Library/LaunchAgents"))
        .iter()
        .filter(|p| {
            p.to_string_lossy().contains("csr-engine")
                && p.extension().is_some_and(|e| e == "plist")
        })
    {
        if let Ok(s) = std::fs::read_to_string(p) {
            if s.contains("<key>EnvironmentVariables</key>") {
                for c in re.captures_iter(&s) {
                    switches.push((c[1].to_owned(), c[2].to_owned(), "daemon"));
                }
            }
        }
    }
    let settings = read_json(&paths.home.join(".claude/settings.json"));
    if let Some(env) = settings["env"].as_object() {
        for (k, v) in env {
            switches.push((
                k.clone(),
                v.as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| v.to_string()),
                "hooks",
            ));
        }
    }
    for (k, v, source) in switches {
        if k.starts_with("CSR_NO_")
            && k != "CSR_NO_RATIFICATION"
            && !matches!(v.trim().to_lowercase().as_str(), "" | "0" | "false")
        {
            inop.push(json!([
                &k[7..],
                format!(
                    "INHIBITED · {k}={} ({source})",
                    super::render::text::one_line(&v, 24)
                ),
                "advisory",
            ]));
        }
    }
    let result = json!({
        "sev": if !up {
            json!("amber")
        } else if inop.is_empty() {
            Value::Null
        } else {
            json!("advisory")
        },
        "inop": inop,
        "daemon": {
            "up": up,
            "pid": if up { pid } else { None },
            "since": if up {
                std::fs::metadata(paths.home.join(".claude-self-reflect/daemon.lock"))
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|t| t.as_secs_f64())
            } else {
                None
            },
        },
    });
    if let Ok(mut cache) = CACHE.lock() {
        cache.insert(paths.home.clone(), (now, result.clone()));
    }
    result
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
        let dir = p.keep.join("statusline");
        std::fs::create_dir_all(&dir).unwrap();
        for (id, at, used) in [("a", 9990, 12), ("b", 9995, 34), ("c", 0, 99)] {
            std::fs::write(
                dir.join(format!("{id}.json")),
                json!({
                    "at": at,
                    "fields": {
                        "rate_limits": {
                            "five_hour": {
                                "used_percentage": used,
                            },
                        },
                    },
                })
                .to_string(),
            )
            .unwrap();
        }
        assert_eq!(
            account_limits(&p, 10000.)["five_hour"]["used_percentage"],
            34
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
        let f = p.kept_file("statusline", "s").unwrap();
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(
            f,
            json!({
                "at": 1000,
                "fields": {
                    "context_window": {
                        "total_input_tokens": 40000,
                    },
                },
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            kept_status(&p, "s", 1100.)["fields"]["context_window"]["total_input_tokens"],
            40000
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
        let dir = paths.keep.join("statusline");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.json"),
            json!({
                "at": 9990,
                "fields": {
                    "rate_limits": {
                        "five_hour": {
                            "used_percentage": 34,
                        },
                    },
                },
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            dir.join("b.json"),
            json!({
                "at": 9995,
                "fields": {
                    "rate_limits": {},
                },
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            account_limits(&paths, 10000.)["five_hour"]["used_percentage"],
            34
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
pub fn compacting_info(paths: &Paths, sid: &str, now: f64, persist: bool) -> Value {
    let marker = paths
        .kept_file("compacting", sid)
        .map(|p| read_json(&p))
        .unwrap_or(Value::Null);
    let at = marker["at"]
        .as_f64()
        .or_else(|| marker["at"].as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0.);
    let at = if at > 1e11 { at / 1000. } else { at };
    if at == 0. || !at.is_finite() || now - at > 600. {
        return Value::Null;
    }
    type Cache = std::collections::HashMap<std::path::PathBuf, (f64, Value)>;
    static CACHE: std::sync::LazyLock<std::sync::Mutex<Cache>> =
        std::sync::LazyLock::new(|| std::sync::Mutex::new(Cache::new()));
    let usual = CACHE
        .lock()
        .ok()
        .and_then(|c| {
            c.get(&paths.home)
                .filter(|(at, _)| now - at < 300.)
                .map(|(_, v)| v.clone())
        })
        .unwrap_or_else(|| {
            let cache = paths.keep.join("compaction-times.json");
            let kept = read_json(&cache);
            let mut found = vec![];
            for folder in folders(&paths.projects) {
                for path in folders(&folder) {
                    if path.extension().is_some_and(|s| s == "jsonl") {
                        if let Some(modified) = std::fs::metadata(&path)
                            .ok()
                            .and_then(|m| m.modified().ok())
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        {
                            found.push((modified.as_nanos() as u64, path));
                        }
                    }
                }
            }
            found.sort_by(|a, b| b.cmp(a));
            found.truncate(60);
            let mut current = json!({});
            let mut seconds = vec![];
            use std::io::BufRead;
            for (modified, path) in found {
                let key = path.to_string_lossy();
                let mut entry = kept[key.as_ref()].clone();
                if array(&entry).len() != 2 || entry[0].as_u64() != Some(modified) {
                    let Ok(file) = std::fs::File::open(&path) else {
                        continue;
                    };
                    let mut reader = std::io::BufReader::new(file);
                    let mut line = vec![];
                    let mut took = vec![];
                    loop {
                        line.clear();
                        match reader.read_until(b'\n', &mut line) {
                            Ok(0) | Err(_) => break,
                            Ok(_) => (),
                        };
                        if !line.windows(18).any(|w| w == b"\"compact_boundary\"") {
                            continue;
                        }
                        if let Ok(row) = sonic_rs::from_slice::<Value>(&line) {
                            if row["subtype"] == "compact_boundary" {
                                if let Some(ms) = row["compactMetadata"]["durationMs"]
                                    .as_f64()
                                    .filter(|n| *n > 0.)
                                {
                                    took.push(ms / 1000.);
                                }
                            }
                        }
                    }
                    entry = json!([modified, took]);
                }
                seconds.extend(array(&entry[1]).iter().filter_map(Value::as_f64));
                current[key.as_ref()] = entry;
            }
            if persist && current != kept {
                let _ = super::private::write_atomic(
                    &cache,
                    &super::private::dumps(&current),
                    &paths.keep,
                );
            }
            seconds.sort_by(f64::total_cmp);
            let usual = if seconds.len() < 3 {
                Value::Null
            } else {
                let pct = |share: f64| {
                    let at = (seconds.len() - 1) as f64 * share;
                    let i = at as usize;
                    seconds[i]
                        + (seconds[(i + 1).min(seconds.len() - 1)] - seconds[i]) * (at - i as f64)
                };
                json!({
                    "low": pct(0.1),
                    "median": pct(0.5),
                    "high": pct(0.9),
                    "n": seconds.len(),
                })
            };
            if let Ok(mut cache) = CACHE.lock() {
                cache.insert(paths.home.clone(), (now, usual.clone()));
            }
            usual
        });
    json!({
        "elapsed": (now - at).max(0.).floor(),
        "trigger": marker["trigger"].as_str().unwrap_or(""),
        "usual": usual,
        "facts": [],
    })
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
