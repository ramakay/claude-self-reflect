//! The Antigravity CLI's (`agy`) plan quota, for the pane's limit bars beside Claude's and Codex's.
//!
//! `agy -p /usage --output-format json` prints the account's quota groups (Gemini models, Claude
//! and GPT models), each with a weekly and a five-hour bucket that says what fraction is left and
//! when it resets; `agy -p /credits` prints the AI credit balance. Each call takes about 3 s and
//! goes to the network, so the frame never runs it: the frame reads `<keep>/agy-usage.json`, and
//! when that file is older than `REFRESH` a detached thread asks agy again and rewrites it (tmp +
//! rename). A stamp file claimed with `create_new` keeps every pane but one from asking at once.
//!
//! The reading only runs when `<home>/.gemini/antigravity-cli` exists and an `agy` binary is found
//! (`CSR_AGY_BIN`, then `PATH`, then `~/.local/bin/agy`): a home with no Antigravity login never
//! starts it. `CSR_NO_AGY=1` turns it off. A reading older than `SHOWN_FOR` is not drawn: unknown
//! is not zero.
use super::contract::Limit;
use super::paths::Paths;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const FILE: &str = "agy-usage.json";
const STAMP: &str = "agy-usage.asking";
const LOG: &str = "agy-usage.log";
/// Seconds between readings.
const REFRESH: f64 = 300.;
/// A reading older than this is not drawn.
const SHOWN_FOR: f64 = 3600.;
/// A stamp older than this was left by a reader that died; it is taken over.
const STAMP_STALE: f64 = 120.;
const TIMEOUT: Duration = Duration::from_secs(30);

/// `limits` (the account's Claude windows) with the newest agy reading under `"agy"`, when there
/// is one fresh enough to draw. Asks agy again, off the frame, when the reading is due.
pub fn with(mut limits: Value, paths: &Paths, now: f64) -> Value {
    if std::env::var_os("CSR_NO_AGY").is_some_and(|v| !v.is_empty() && v != "0") {
        return limits;
    }
    let file = paths.keep.join(FILE);
    let kept = super::paths::read_json(&file);
    let at = kept["at"].as_f64();
    if at.is_none_or(|at| now - at >= REFRESH || now < at) {
        ask_later(paths, now);
    }
    if at.is_some_and(|at| now - at < SHOWN_FOR && now >= at - 60.) && kept["groups"].is_array() {
        if !limits.is_object() {
            limits = json!({});
        }
        limits["agy"] = kept;
    }
    limits
}

/// The agy binary to run, if this home has an Antigravity login and the binary is found.
fn binary(home: &Path) -> Option<PathBuf> {
    if !home.join(".gemini/antigravity-cli").is_dir() {
        return None;
    }
    if let Some(bin) = std::env::var_os("CSR_AGY_BIN").filter(|b| !b.is_empty()) {
        return Some(bin.into());
    }
    let on_path = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default();
    on_path
        .into_iter()
        .chain([home.join(".local/bin")])
        .map(|d| d.join("agy"))
        .find(|p| p.is_file())
}

/// Starts one detached reading unless another pane is already taking one.
fn ask_later(paths: &Paths, now: f64) {
    if cfg!(test) {
        return;
    }
    let Some(bin) = binary(&paths.home) else {
        return;
    };
    let stamp = paths.keep.join(STAMP);
    if !claim(&stamp, now) {
        return;
    }
    let keep = paths.keep.clone();
    std::thread::spawn(move || {
        if let Some(reading) = read_agy(&bin, &keep) {
            let tmp = keep.join(format!("{FILE}.{}", std::process::id()));
            if std::fs::write(&tmp, reading.to_string()).is_ok() {
                let _ = std::fs::rename(&tmp, keep.join(FILE));
            }
        }
        let _ = std::fs::remove_file(&stamp);
    });
}

/// Takes the stamp file, or a stale one; false while another reader holds a fresh one.
fn claim(stamp: &Path, now: f64) -> bool {
    let open = || {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(stamp)
    };
    if open().is_ok() {
        return true;
    }
    let age = std::fs::metadata(stamp)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(f64::MAX, |d| now - d.as_secs_f64());
    age > STAMP_STALE && std::fs::remove_file(stamp).is_ok() && open().is_ok()
}

/// One `agy -p <command> --output-format json` run's `command.data`, or nothing on any failure.
fn run(bin: &Path, keep: &Path, command: &str) -> Option<Value> {
    let mut child = Command::new(bin)
        .args(["-p", command, "--output-format", "json", "--log-file"])
        .arg(keep.join(LOG))
        .current_dir(keep)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut out = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut out, &mut text).ok()?;
        Some(text)
    });
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < TIMEOUT => std::thread::sleep(Duration::from_millis(100)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let text = reader.join().ok()??;
    let reply: Value = serde_json::from_str(&text).ok()?;
    (reply["status"] == "SUCCESS").then(|| reply["command"]["data"].clone())
}

fn read_agy(bin: &Path, keep: &Path) -> Option<Value> {
    let usage = run(bin, keep, "/usage")?;
    let credits = run(bin, keep, "/credits");
    Some(reading(&usage, credits.as_ref(), now_secs()))
}

fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0., |d| d.as_secs_f64())
}

/// The kept reading: each group's buckets as `{window, left, resets}` (`left` a fraction,
/// `resets` an epoch second or null when agy gives none), and the AI credit balance.
pub fn reading(usage: &Value, credits: Option<&Value>, at: f64) -> Value {
    let groups: Vec<Value> = super::array(&usage["groups"])
        .iter()
        .map(|g| {
            let buckets: Vec<Value> = super::array(&g["buckets"])
                .iter()
                .filter_map(|b| {
                    let left = b["remaining_fraction"].as_f64()?;
                    let resets = b["reset_time"]
                        .as_str()
                        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                        .map(|t| t.timestamp() as f64)
                        .filter(|t| *t > 0.);
                    Some(json!({
                        "window": super::string(&b["window"]),
                        "left": left.clamp(0., 1.),
                        "resets": resets,
                    }))
                })
                .collect();
            json!({"name": super::string(&g["name"]), "buckets": buckets})
        })
        .collect();
    json!({
        "at": at,
        "groups": groups,
        "credits": credits.and_then(|c| c["remaining_credits"].as_f64()),
    })
}

/// The bars the limits card draws for a reading: one per group in use (a bucket below full or
/// with a reset still ahead), showing the bucket with the least left. A bucket whose reset has
/// passed is not drawn: it has refilled since the reading.
pub fn limits(agy: &Value, now: f64) -> Vec<Limit> {
    let credits = agy["credits"].as_f64().filter(|n| *n > 0.);
    let mut out = vec![];
    for g in super::array(&agy["groups"]) {
        let buckets: Vec<(&str, f64, Option<f64>)> = super::array(&g["buckets"])
            .iter()
            .filter_map(|b| {
                Some((
                    b["window"].as_str()?,
                    b["left"].as_f64()?,
                    b["resets"].as_f64(),
                ))
            })
            .filter(|(_, _, resets)| resets.is_none_or(|r| r > now))
            .collect();
        if !buckets
            .iter()
            .any(|(_, left, resets)| *left < 1. || resets.is_some())
        {
            continue;
        }
        let Some(&(window, left, resets)) = buckets.iter().min_by(|a, b| {
            a.1.total_cmp(&b.1)
                .then_with(|| (a.0 != "weekly").cmp(&(b.0 != "weekly")))
        }) else {
            continue;
        };
        let used = ((1. - left) * 100.).clamp(0., 100.);
        let code = if window == "weekly" { "7D" } else { "5H" };
        let group = match super::string(&g["name"]) {
            "Gemini Models" => "Gemini",
            "Claude and GPT models" => "Claude/GPT",
            other => other,
        };
        let reset = resets
            .map(|r| format!(" · resets in {}", super::forecast::until(r - now)))
            .unwrap_or_default();
        let credit = credits
            .map(|n| format!(" · {n:.0} AI credits"))
            .unwrap_or_default();
        out.push(Limit {
            code: format!("AGY{code}"),
            name: format!("agy {code}"),
            used,
            sev: if used >= 90. {
                "red"
            } else if used >= 75. {
                "amber"
            } else {
                "ok"
            }
            .to_owned(),
            tick: None,
            line: format!(
                "agy {group} · {}% left{reset}{credit}",
                (left * 100.).round() as i64
            ),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11-25 23:20Z: before both Gemini resets in `usage()`.
    const NOW: f64 = 1_795_650_000.;

    fn usage() -> Value {
        json!({"groups": [
            {"name": "Gemini Models", "buckets": [
                {"id": "gemini-weekly", "window": "weekly", "remaining_fraction": 0.8484677,
                 "reset_time": "2026-11-30T20:05:37Z"},
                {"id": "gemini-5h", "window": "5h", "remaining_fraction": 1,
                 "reset_time": "2026-11-26T07:31:49Z"}]},
            {"name": "Claude and GPT models", "buckets": [
                {"id": "3p-weekly", "window": "weekly", "remaining_fraction": 1,
                 "reset_time": "1970-01-01T00:00:00Z"},
                {"id": "3p-5h", "window": "5h", "remaining_fraction": 1,
                 "reset_time": "1970-01-01T00:00:00Z"}]}]})
    }

    #[test]
    fn the_reading_keeps_each_bucket_and_drops_the_1970_reset() {
        let r = reading(&usage(), Some(&json!({"remaining_credits": 0})), NOW);
        assert_eq!(r["at"], NOW);
        assert_eq!(r["groups"][0]["buckets"][0]["window"], "weekly");
        assert!(r["groups"][0]["buckets"][0]["resets"].as_f64().unwrap() > NOW);
        assert_eq!(r["groups"][1]["buckets"][0]["resets"], Value::Null);
        assert_eq!(r["credits"], 0.);
    }

    #[test]
    fn a_group_in_use_gets_one_bar_for_its_tightest_window() {
        let r = reading(&usage(), None, NOW);
        let bars = limits(&r, NOW);
        assert_eq!(bars.len(), 1, "the unused Claude/GPT group draws no bar");
        assert_eq!(bars[0].name, "agy 7D");
        assert_eq!(bars[0].used.round(), 15.);
        assert_eq!(bars[0].sev, "ok");
        assert!(bars[0]
            .line
            .starts_with("agy Gemini · 85% left · resets in "));
        assert!(
            !bars[0].line.contains("credits"),
            "a zero balance is not shown"
        );
    }

    #[test]
    fn a_lower_five_hour_window_wins_and_credits_show_when_above_zero() {
        let mut u = usage();
        u["groups"][0]["buckets"][1]["remaining_fraction"] = json!(0.05);
        let r = reading(&u, Some(&json!({"remaining_credits": 120})), NOW);
        let bars = limits(&r, NOW);
        assert_eq!(bars[0].name, "agy 5H");
        assert_eq!(bars[0].sev, "red");
        assert!(bars[0].line.contains("5% left"));
        assert!(bars[0].line.ends_with(" · 120 AI credits"));
    }

    #[test]
    fn a_bucket_past_its_reset_is_not_drawn() {
        let r = reading(&usage(), None, NOW);
        assert!(limits(&r, NOW + 30. * 86400.).is_empty());
    }

    #[test]
    fn with_adds_a_fresh_reading_and_leaves_an_old_one_out() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().to_path_buf());
        std::fs::create_dir_all(&paths.keep).unwrap();
        assert_eq!(with(json!({}), &paths, NOW), json!({}), "no file, no key");
        let r = reading(&usage(), None, NOW - 60.);
        std::fs::write(paths.keep.join(FILE), r.to_string()).unwrap();
        let got = with(json!({"five_hour": {"used_percentage": 3}}), &paths, NOW);
        assert_eq!(got["five_hour"]["used_percentage"], 3);
        assert_eq!(got["agy"]["groups"][0]["name"], "Gemini Models");
        assert_eq!(with(json!({}), &paths, NOW + SHOWN_FOR), json!({}));
    }

    #[test]
    fn a_home_with_no_antigravity_login_finds_no_binary() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(binary(dir.path()), None);
    }

    #[test]
    fn a_fresh_stamp_holds_and_a_stale_one_is_taken_over() {
        let dir = tempfile::tempdir().unwrap();
        let stamp = dir.path().join(STAMP);
        assert!(claim(&stamp, now_secs()));
        assert!(!claim(&stamp, now_secs()));
        assert!(claim(&stamp, now_secs() + STAMP_STALE + 5.));
    }
}
