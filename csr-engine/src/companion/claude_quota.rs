//! Claude's per-model weekly limits (Fable's share of the week) for the limits chart.
//!
//! Claude Code's `session.measure` event carries only the five-hour and seven-day windows. The
//! per-model rows `/usage` shows ("Current week (Fable)") come only from the account's
//! `GET https://api.anthropic.com/api/oauth/usage`, the endpoint Claude Code itself reads: its
//! `limits` list has a `weekly_scoped` row per model, and `weekly_scoped_shares` says how many of
//! the week's points that model may take and has taken.
//!
//! # The token
//!
//! * It is Claude Code's own OAuth access token, read from the login Keychain (service
//!   `Claude Code-credentials`) by `/usr/bin/security` in the fetch thread and handed to `curl`
//!   on stdin: never in argv, an environment, a file, a log or the cache.
//! * An expired token is left alone, never refreshed: the next fetch after Claude Code refreshes
//!   it succeeds.
//! * The cache (`<keep>/claude-usage.json`) holds the model names, percents and reset times only.
//!
//! One fetch per `REFRESH` across every pane (the stamp claim agy.rs uses), off the frame.
//! `CSR_NO_CLAUDE_QUOTA=1` turns it off. A reading older than `SHOWN_FOR` is not drawn.
use super::contract::{sev_for, Limit};
use super::paths::Paths;
use serde_json::{json, Value};
use std::process::{Command, Stdio};

pub const FILE: &str = "claude-usage.json";
const STAMP: &str = "claude-usage.asking";
/// Seconds between fetches.
const REFRESH: f64 = 300.;
/// A reading older than this is not drawn.
const SHOWN_FOR: f64 = 3600.;
const SERVICE: &str = "Claude Code-credentials";
const ENDPOINT: &str = "https://api.anthropic.com/api/oauth/usage?skip_spend=1";

/// `limits` (the account's Claude windows) with the agy reading (agy.rs) and Claude's per-model
/// weekly rows under `"scoped"`, when they are fresh enough to draw. `ask` is false for a pane
/// that keeps nothing (a `--png`, `--json` or `--frame` export): it reads, never fetches.
pub fn with(limits: Value, paths: &Paths, now: f64, ask: bool) -> Value {
    let mut limits = super::agy::with(limits, paths, now, ask);
    if std::env::var_os("CSR_NO_CLAUDE_QUOTA").is_some_and(|v| !v.is_empty() && v != "0") {
        return limits;
    }
    let kept = super::paths::read_json(&paths.keep.join(FILE));
    if ask
        && kept["asked"]
            .as_f64()
            .is_none_or(|at| now - at >= REFRESH || now < at)
    {
        ask_later(paths, now);
    }
    let at = kept["at"].as_f64();
    if at.is_some_and(|at| now - at < SHOWN_FOR && now >= at - 60.) && kept["scoped"].is_array() {
        if !limits.is_object() {
            limits = json!({});
        }
        limits["scoped"] = kept["scoped"].clone();
    }
    limits
}

/// Starts one detached fetch unless another pane is already making one. A failed fetch keeps the
/// last reading (it ages out by `SHOWN_FOR`) and records when it asked, so it waits `REFRESH`.
fn ask_later(paths: &Paths, now: f64) {
    if cfg!(test) || !cfg!(target_os = "macos") {
        return;
    }
    let stamp = paths.keep.join(STAMP);
    if !super::agy::claim(&stamp, now) {
        return;
    }
    let keep = paths.keep.clone();
    std::thread::spawn(move || {
        let now = super::agy::now_secs();
        let file = keep.join(FILE);
        let mut kept = super::paths::read_json(&file);
        if let Some(body) = token(now).and_then(|t| fetch(&t)) {
            kept = reading(&body, now);
        }
        if !kept.is_object() {
            kept = json!({});
        }
        kept["asked"] = json!(now);
        let tmp = keep.join(format!("{FILE}.{}", std::process::id()));
        if std::fs::write(&tmp, kept.to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, &file);
        }
        let _ = std::fs::remove_file(&stamp);
    });
}

/// Claude Code's OAuth access token, if the Keychain has one that is not about to expire.
/// The item under the user's own account first: the service can hold another item (the MCP
/// servers' tokens) with no Claude login in it.
fn token(now: f64) -> Option<String> {
    let user = std::env::var("USER").unwrap_or_default();
    token_of(&user, now).or_else(|| token_of("", now))
}

fn token_of(acct: &str, now: f64) -> Option<String> {
    let mut cmd = Command::new("/usr/bin/security");
    cmd.args(["find-generic-password", "-s", SERVICE]);
    if !acct.is_empty() {
        cmd.args(["-a", acct]);
    }
    let out = cmd
        .arg("-w")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let creds: Value = serde_json::from_slice(&out.stdout).ok()?;
    let oauth = &creds["claudeAiOauth"];
    let fresh = oauth["expiresAt"]
        .as_f64()
        .is_none_or(|ms| ms / 1000. > now + 60.);
    let token = oauth["accessToken"].as_str()?.trim();
    (fresh && !token.is_empty()).then(|| token.to_owned())
}

/// The endpoint's answer, or nothing on any failure. The token goes to curl on stdin.
fn fetch(token: &str) -> Option<Value> {
    use std::io::Write;
    let mut child = Command::new("curl")
        .args(["-s", "--connect-timeout", "8", "-m", "20"])
        .args(["-w", "\n%{http_code}", "-H", "@-"])
        .args(["-H", "anthropic-beta: oauth-2025-04-20"])
        .args(["-A", concat!("csr-engine/", env!("CARGO_PKG_VERSION"))])
        .arg(ENDPOINT)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = writeln!(stdin, "Authorization: Bearer {token}");
    }
    let out = child.wait_with_output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let (body, code) = text.rsplit_once('\n')?;
    (code.trim() == "200")
        .then(|| serde_json::from_str(body).ok())
        .flatten()
}

/// The kept reading: one row per model with its own weekly limit, as `{model, used, resets,
/// of_week, allowance}` (`used` its own percent; `of_week` the week's points it has taken and
/// `allowance` the most it may take, when the endpoint says).
pub fn reading(body: &Value, at: f64) -> Value {
    let shares = super::array(&body["weekly_scoped_shares"]);
    let scoped: Vec<Value> = super::array(&body["limits"])
        .iter()
        .enumerate()
        .filter(|(_, l)| l["kind"] == "weekly_scoped")
        .filter_map(|(i, l)| {
            let model = l["scope"]["model"]["display_name"].as_str()?;
            let share = shares
                .iter()
                .find(|s| s["limit_index"].as_u64() == Some(i as u64));
            Some(json!({
                "model": model,
                "used": l["percent"].as_f64()?,
                "resets": l["resets_at"].as_str().and_then(super::paths::epoch),
                "of_week": share.and_then(|s| s["used_percent_of_weekly"].as_f64()),
                "allowance": share.and_then(|s| s["allowance_percent_of_weekly"].as_f64()),
            }))
        })
        .collect();
    json!({"at": at, "scoped": scoped})
}

/// Puts a column per model with its own weekly limit after Claude's windows, and stacks each
/// model's points into the Weekly column, so the week reads as Fable's part and the rest.
pub fn fold(out: &mut Vec<Limit>, scoped: &Value, now: f64) {
    let rows: Vec<&Value> = super::array(scoped)
        .iter()
        .filter(|r| r["model"].is_string())
        .filter(|r| r["resets"].as_f64().is_none_or(|t| t > now))
        .collect();
    if rows.is_empty() {
        return;
    }
    if let Some(week) = out
        .iter_mut()
        .find(|l| l.group == "Claude" && l.code == "7D")
    {
        let mut rest = week.used;
        for r in &rows {
            if let Some(points) = r["of_week"].as_f64() {
                let points = points.clamp(0., rest.max(0.));
                week.parts
                    .push((super::string(&r["model"]).to_owned(), points));
                rest -= points;
            }
        }
        if !week.parts.is_empty() {
            week.parts.push(("other models".into(), rest.max(0.)));
        }
    }
    let cols = rows.iter().map(|r| {
        let model = super::string(&r["model"]);
        let used = super::num(&r["used"]).clamp(0., 100.);
        let back = r["resets"]
            .as_f64()
            .map(|t| super::forecast::until(t - now));
        let points = match (r["of_week"].as_f64(), r["allowance"].as_f64()) {
            (Some(_), Some(all)) if used >= 100. => {
                format!(" · all {} of its weekly points", all.round() as i64)
            }
            (Some(of), Some(all)) => format!(
                " · {} of its {} weekly points",
                of.round() as i64,
                all.round() as i64
            ),
            _ => String::new(),
        };
        let line = match back {
            Some(b) if used >= 100. => format!("{model} · used up{points} · back in {b}"),
            Some(b) => format!(
                "{model} · {}% used{points} · resets in {b}",
                used.round() as i64
            ),
            None => format!("{model} · {}% used{points}", used.round() as i64),
        };
        Limit {
            code: format!("{}7D", model.to_uppercase()),
            name: format!("{model} 7D"),
            used,
            sev: sev_for(used),
            line,
            group: "Claude".into(),
            label: model.to_owned(),
            ..Limit::default()
        }
    });
    let at = out
        .iter()
        .rposition(|l| l.group == "Claude")
        .map_or(0, |i| i + 1);
    out.splice(at..at, cols.collect::<Vec<_>>());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11-26 17:15Z, before the week's reset in `body()`.
    const NOW: f64 = 1_795_713_300.;

    /// The endpoint's shape on 2026-11-26 (other fields dropped).
    fn body() -> Value {
        json!({
            "limits": [
                {"kind": "weekly_all", "percent": 77, "resets_at": "2026-11-29T13:59:59.836722+00:00",
                 "scope": null},
                {"kind": "weekly_scoped", "percent": 100,
                 "resets_at": "2026-11-29T13:59:59.836980+00:00",
                 "scope": {"model": {"id": null, "display_name": "Fable"}, "surface": null}}
            ],
            "weekly_scoped_shares": [
                {"limit_index": 1, "used_percent_of_weekly": 50, "allowance_percent_of_weekly": 50}
            ]
        })
    }

    fn week(used: f64) -> Vec<Limit> {
        vec![
            Limit {
                code: "5H".into(),
                group: "Claude".into(),
                ..Limit::default()
            },
            Limit {
                code: "7D".into(),
                used,
                group: "Claude".into(),
                ..Limit::default()
            },
            Limit {
                code: "CX".into(),
                group: "Codex".into(),
                ..Limit::default()
            },
        ]
    }

    #[test]
    fn the_reading_keeps_each_scoped_model_with_its_share_of_the_week() {
        let r = reading(&body(), NOW);
        assert_eq!(r["at"], NOW);
        let rows = r["scoped"].as_array().unwrap();
        assert_eq!(
            rows.len(),
            1,
            "the all-models row is the session's own Weekly"
        );
        assert_eq!(rows[0]["model"], "Fable");
        assert_eq!(rows[0]["used"], 100.);
        assert_eq!(rows[0]["of_week"], 50.);
        assert_eq!(rows[0]["allowance"], 50.);
        assert!(rows[0]["resets"].as_f64().unwrap() > NOW);
    }

    #[test]
    fn a_used_up_model_gets_its_column_and_its_points_stack_into_the_week() {
        let mut out = week(77.);
        fold(&mut out, &reading(&body(), NOW)["scoped"], NOW);
        let codes: Vec<_> = out.iter().map(|l| l.code.as_str()).collect();
        assert_eq!(codes, ["5H", "7D", "FABLE7D", "CX"]);
        assert_eq!(
            out[1].parts,
            vec![("Fable".to_owned(), 50.), ("other models".to_owned(), 27.)]
        );
        let fable = &out[2];
        assert_eq!((fable.label.as_str(), fable.sev.as_str()), ("Fable", "red"));
        assert!(
            fable
                .line
                .starts_with("Fable · used up · all 50 of its weekly points · back in "),
            "{}",
            fable.line
        );
    }

    #[test]
    fn a_model_part_way_through_says_its_points_and_a_past_reset_drops_it() {
        let mut b = body();
        b["limits"][1]["percent"] = json!(40);
        b["weekly_scoped_shares"][0]["used_percent_of_weekly"] = json!(20);
        let scoped = &reading(&b, NOW)["scoped"];
        let mut out = week(30.);
        fold(&mut out, scoped, NOW);
        assert!(out[2]
            .line
            .starts_with("Fable · 40% used · 20 of its 50 weekly points · resets in "));
        assert_eq!(out[2].sev, "ok");
        let mut later = week(30.);
        fold(&mut later, scoped, NOW + 7. * 86400.);
        assert_eq!(later, week(30.), "a reset that has passed draws nothing");
    }

    #[test]
    fn with_adds_a_fresh_reading_and_leaves_an_old_one_out() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().to_path_buf());
        std::fs::create_dir_all(&paths.keep).unwrap();
        assert_eq!(
            with(json!({}), &paths, NOW, false),
            json!({}),
            "no file, no key"
        );
        let r = reading(&body(), NOW - 60.);
        std::fs::write(paths.keep.join(FILE), r.to_string()).unwrap();
        let got = with(
            json!({"seven_day": {"used_percentage": 77}}),
            &paths,
            NOW,
            false,
        );
        assert_eq!(got["seven_day"]["used_percentage"], 77);
        assert_eq!(got["scoped"][0]["model"], "Fable");
        assert_eq!(with(json!({}), &paths, NOW + SHOWN_FOR, false), json!({}));
    }
}
