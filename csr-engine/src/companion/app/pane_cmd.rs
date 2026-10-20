//! The `/csr` command, the pane, its refresh, and the pane opened beside Claude (register.tsx
//! 2384-2431, 2485-2739).
//!
//! - `start` has two entries. The first (`register`) clears the status line and registers the
//!   command, for a person's session only (`isInteractive` true). The late one (`register_late`)
//!   sets the 10-minute refresh and, in the background, refreshes once and opens the pane when the
//!   folder is a repository.
//! - `csr` routes by the first word: `dreams`, `setup`, `statusline` (retired: one line), or the
//!   default, which refreshes, opens the pane, and counts.
//! - `pane` draws the cards (`pane` submodule); its dreams offer asks once before it turns dreams
//!   on, so the press is a `press` entry too.
//!
//! The status line is retired (owner, 2026-10-19: the masthead takes its figures from
//! `session.measure`, and `hook install --apply` unwraps old setups): no wrapper, no
//! `/csr statusline` route, no hint in the help or the pane.
//!
//! Work outside this process (the shifts data, the herdr splits, the environment) goes through
//! `Outside`, so a test brings its own. `Real` runs the pane's `--json` mode in process and herdr
//! as a child; the engine never spawns itself.

mod dreams_text;
mod pane;
#[cfg(test)]
mod tests;

use super::{
    setup_walk,
    state::{config_row, config_rows, update, Options},
};
use crate::companion::relay::{
    dispatch::{handler, Ctx, Handler, Reply, Table, PRESS},
    host::Call,
};
use anyhow::Result;
use async_trait::async_trait;
use pane::TurnedOn;
use serde_json::{json, Value};
use std::{path::Path, sync::Arc, time::Duration};

/// How often the shifts data is read again (register.tsx `REFRESH_MS`).
pub const REFRESH_MS: f64 = 10. * 60. * 1000.;
/// What a read of the shifts data may take (the TypeScript gave `pane.py` 20 s).
const SHIFTS_LIMIT: Duration = Duration::from_secs(20);
/// What one herdr command may take.
const HERDR_LIMIT: Duration = Duration::from_secs(10);
const LABEL: &str = "CSR";
const RETIRED: &str = "The status line wrapper is gone: the pane's masthead shows context and \
    limits, and \"csr-engine hook install --apply\" already put back any wrapped line.";

/// The pane's module state.
#[derive(Default)]
struct PaneState {
    /// Where the dreams offer stands (register.tsx `turnedOn`).
    turned_on: Option<TurnedOn>,
}

/// What a command run outside came to.
pub struct Ran {
    pub code: i32,
    pub stdout: String,
}

/// What the pane needs from outside this process.
#[async_trait]
pub trait Outside: Send + Sync {
    /// The pane's `--json` data for `cwd` and the session; none when it could not be read in time.
    async fn shifts(&self, cwd: &str, sid: &str) -> Option<Value>;
    /// A variable of the environment Claude Code runs in.
    fn env(&self, name: &str) -> Option<String>;
    /// Run a command; none when it could not run.
    async fn run(&self, argv: Vec<String>) -> Option<Ran>;
    /// The binary a herdr split runs the pane watcher with.
    fn engine(&self) -> Option<String>;
}

/// The real thing.
pub struct Real;

#[async_trait]
impl Outside for Real {
    async fn shifts(&self, cwd: &str, sid: &str) -> Option<Value> {
        let (cwd, sid) = (cwd.to_string(), sid.to_string());
        let read = tokio::task::spawn_blocking(move || {
            let now = std::env::var("CSR_COMPANION_NOW")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(crate::companion::paths::now);
            let paths = crate::companion::paths::Paths::from_env();
            let found =
                crate::companion::hook_json::pane_json(&paths, Path::new(&cwd), &sid, 60, now);
            serde_json::to_value(found).ok()
        });
        tokio::time::timeout(SHIFTS_LIMIT, read).await.ok()?.ok()?
    }

    fn env(&self, name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|v| !v.is_empty())
    }

    async fn run(&self, argv: Vec<String>) -> Option<Ran> {
        let (first, rest) = argv.split_first()?;
        let mut command = tokio::process::Command::new(first);
        command
            .args(rest)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        let out = tokio::time::timeout(HERDR_LIMIT, command.output())
            .await
            .ok()?
            .ok()?;
        Some(Ran {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        })
    }

    fn engine(&self) -> Option<String> {
        let exe = std::env::current_exe().ok()?;
        Some(exe.to_string_lossy().into_owned())
    }
}

pub fn real() -> Arc<dyn Outside> {
    Arc::new(Real)
}

/// Whether a start is a person's: `e.isInteractive` is true (`if (!e.isInteractive) return`). A
/// start that does not say is not one. The features that act later ask `state::is_person`.
fn starts_a_person(e: &Value) -> bool {
    e["isInteractive"] == true
}

/// Read the shifts data again and keep it as the session's `data` row (register.tsx `refresh`).
async fn refresh(cx: &Ctx, outside: &dyn Outside, cwd: &str) -> Option<Value> {
    let fresh = outside.shifts(cwd, &cx.sid).await;
    let _ = update(cx, "data", |_| fresh.clone());
    fresh
}

fn herdr_pane(out: &Ran) -> Option<Value> {
    let found: Value = serde_json::from_str(&out.stdout).ok()?;
    Some(found["result"]["pane"].clone()).filter(|p| p.is_object())
}

/// A herdr split beside this pane: a real side pane in the terminal, where the session strip and
/// the date axes draw in pixels. One per tab: an older pane is closed and opened fresh, so it
/// follows this session.
async fn open_herdr_pane(cx: &Ctx, outside: &dyn Outside, cwd: &str) -> bool {
    let Some(me) = outside.env("HERDR_PANE_ID") else {
        return false;
    };
    let herdr = outside
        .env("HERDR_BIN_PATH")
        .unwrap_or_else(|| "herdr".into());
    let Some(binary) = outside.engine() else {
        return false;
    };
    let run = |args: &[&str]| {
        let mut argv = vec![herdr.clone()];
        argv.extend(args.iter().map(|a| a.to_string()));
        outside.run(argv)
    };
    // Claude's own space and tab, asked of herdr by its pane id: the old CSR pane is looked for
    // there, whichever space you are looking at when the session starts.
    let got = run(&["pane", "get", &me]).await;
    let mine = got.filter(|g| g.code == 0).and_then(|g| herdr_pane(&g));
    let Some(tab) = mine
        .as_ref()
        .and_then(|m| m["tab_id"].as_str())
        .filter(|t| !t.is_empty())
    else {
        return false;
    };
    let space = match mine.as_ref().and_then(|m| m["workspace_id"].as_str()) {
        Some(space) => space.to_string(),
        None => tab.split(':').next().unwrap_or("").to_string(),
    };
    let Some(listed) = run(&["pane", "list", "--workspace", &space])
        .await
        .filter(|l| l.code == 0)
    else {
        return false;
    };
    let Ok(listed) = serde_json::from_str::<Value>(&listed.stdout) else {
        return false;
    };
    for pane in listed["result"]["panes"].as_array().into_iter().flatten() {
        let label = pane["label"].as_str();
        if pane["tab_id"].as_str() == Some(tab) && matches!(label, Some(LABEL | "Shifts")) {
            if let Some(id) = pane["pane_id"].as_str() {
                run(&["pane", "close", id]).await;
            }
        }
    }
    let split = run(&[
        "pane",
        "split",
        "--pane",
        &me,
        "--direction",
        "right",
        "--ratio",
        "0.62",
        "--cwd",
        cwd,
        "--no-focus",
    ])
    .await;
    let id = split
        .filter(|s| s.code == 0)
        .and_then(|s| serde_json::from_str::<Value>(&s.stdout).ok())
        .and_then(|v| v["result"]["pane"]["pane_id"].as_str().map(str::to_string))
        .filter(|id| !id.is_empty());
    let Some(id) = id else {
        return false;
    };
    run(&["pane", "rename", &id, LABEL]).await;
    // The engine's watch catches a failing frame and re-execs when its binary changes, so the
    // split survives both a broken frame and an engine update.
    run(&[
        "pane",
        "run",
        &id,
        &binary,
        "companion",
        "pane",
        "--repo",
        cwd,
        "--watch",
        "--sid",
        &cx.sid,
        "--agent",
        &me,
    ])
    .await;
    true
}

/// Open the pane: a herdr split when there is one, else Claude Code's own pane. Says which.
async fn open_pane(cx: &Ctx, outside: &dyn Outside, cwd: &str) -> &'static str {
    if open_herdr_pane(cx, outside, cwd).await {
        return "herdr";
    }
    let _ = cx
        .host
        .call("ui.open", vec![json!({ "id": "csr", "title": LABEL })])
        .await;
    "pane"
}

fn has_repo(fresh: &Option<Value>) -> bool {
    fresh
        .as_ref()
        .and_then(|f| f["repo"].as_str())
        .is_some_and(|r| !r.is_empty())
}

fn on_start() -> Handler {
    handler(|cx, e| async move {
        if !starts_a_person(&e) || cx.respawn {
            // The registration lives in the module, not in the child that died.
            return Ok(Reply::none());
        }
        let command = json!({
            "name": "csr",
            "description": "Open the CSR pane beside Claude; \"/csr setup\" walks you through \
                its settings; \"/csr dreams\" says how the dreamer is pacing itself",
        });
        // CSR pins no line under the prompt: the pane carries what it knows. Two independent
        // calls, one round trip.
        let _ = cx
            .host
            .batch(vec![
                Call::new("command.register", vec![command]),
                Call::new("ui.status", vec![]),
            ])
            .await;
        Ok(Reply::none())
    })
}

/// The recurring read first, however the first one ends; then, apart from the start, one read now
/// and the pane opened when the folder is a repository.
fn on_start_late(outside: Arc<dyn Outside>) -> Handler {
    handler(move |cx, e| {
        let outside = outside.clone();
        async move {
            if !starts_a_person(&e) {
                return Ok(Reply::none());
            }
            let cwd = e["cwd"].as_str().unwrap_or(&cx.cwd).to_string();
            {
                let (outside, cwd) = (outside.clone(), cwd.clone());
                cx.every(REFRESH_MS, move |cx| {
                    let (outside, cwd) = (outside.clone(), cwd.clone());
                    async move {
                        refresh(&cx, outside.as_ref(), &cwd).await;
                    }
                });
            }
            if cx.respawn {
                return Ok(Reply::none()); // the pane is open still
            }
            cx.after(0., move |cx| {
                let (outside, cwd) = (outside.clone(), cwd.clone());
                async move {
                    let fresh = refresh(&cx, outside.as_ref(), &cwd).await;
                    if has_repo(&fresh) {
                        open_pane(&cx, outside.as_ref(), &cwd).await;
                    }
                }
            });
            Ok(Reply::none())
        }
    })
}

async fn dreams_on(cx: &Ctx) -> bool {
    Options::of(&config_rows(cx).await).dreams
}

async fn dreams_say(cx: &Ctx) -> String {
    let on = dreams_on(cx).await;
    dreams_text::dreams_text(cx, on)
}

/// A JSON value as `${value}` prints it.
fn shown(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Number(n) => n.as_i64().map_or_else(|| n.to_string(), |n| n.to_string()),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn on_command(outside: Arc<dyn Outside>) -> Handler {
    handler(move |cx, e| {
        let outside = outside.clone();
        async move {
            let Some(args) = e["args"].as_str() else {
                // Not a command run, which always carries its words: the relay's liveness probe.
                let text = format!("csr relay ok pid={} events={}", cx.pid, cx.events);
                return Ok(Reply::answer(json!({ "text": text })));
            };
            let text = match args.split_whitespace().next() {
                Some("dreams") => dreams_say(&cx).await,
                Some("setup") => setup_walk::command_setup(&cx).await?,
                Some("statusline") => RETIRED.to_string(),
                _ => {
                    let cwd = cx.cwd.clone();
                    let fresh = refresh(&cx, outside.as_ref(), &cwd).await;
                    let opened = open_pane(&cx, outside.as_ref(), &cwd).await;
                    let counts = match &fresh {
                        Some(f) => format!(
                            "{} dreams this week, {} changed since your sessions",
                            shown(&f["dreams_7d"]),
                            shown(&f["changes_total"])
                        ),
                        None => "csr-engine could not be read".to_string(),
                    };
                    let where_ = if opened == "herdr" {
                        "CSR pane opened beside this one"
                    } else {
                        "CSR pane opened"
                    };
                    format!("{where_} ({counts}). {}", dreams_say(&cx).await)
                }
            };
            Ok(Reply::answer(json!({ "text": text })))
        }
    })
}

fn on_pane() -> Handler {
    handler(|cx, e| async move {
        let data = cx.store.get(&cx.sid, "data").ok().flatten();
        let lasting = cx.store.get(&cx.sid, "infinite").ok().flatten();
        let on = dreams_on(&cx).await;
        let turned = cx.kept(|s: &mut PaneState| s.turned_on);
        let cols = e["props"]["bodyColumns"].as_f64().unwrap_or(40.).max(0.) as usize;
        let tree = pane::render(data.as_ref(), lasting.as_ref(), on, turned, cols);
        Ok(Reply::tree(tree))
    })
}

/// `$.config.set` came back refused: an error, or a `deny` that says something.
fn denied(done: &Result<Value>) -> bool {
    match done {
        Err(_) => true,
        Ok(done) => done.get("deny").is_some_and(|deny| match deny {
            Value::Null | Value::Bool(false) => false,
            Value::String(text) => !text.is_empty(),
            _ => true,
        }),
    }
}

/// The dreams offer's press. The `/config` row is in your user settings, so it turns dreams on in
/// every session, the other windows too: the first press asks again, as the band's turn-off does.
/// The second changes the row.
fn on_press() -> Handler {
    handler(|cx, e| async move {
        if e["key"].as_str() != Some("dreams-on") {
            return Ok(Reply::none());
        }
        let asked = cx.kept(|s: &mut PaneState| s.turned_on) == Some(TurnedOn::Asked);
        if !asked {
            cx.kept(|s: &mut PaneState| s.turned_on = Some(TurnedOn::Asked));
        } else {
            let key = config_row(&config_rows(&cx).await, "dreams");
            let done = cx
                .host
                .call("config.set", vec![json!({ "key": key, "value": true })])
                .await;
            let turned = if denied(&done) {
                TurnedOn::Refused
            } else {
                TurnedOn::On
            };
            cx.kept(|s: &mut PaneState| s.turned_on = Some(turned));
        }
        let _ = cx.host.call("ui.invalidate", vec![]).await;
        Ok(Reply::none())
    })
}

pub fn register_with(t: &mut Table, outside: Arc<dyn Outside>) {
    t.on("start", "pane_cmd", on_start());
    t.on("csr", "pane_cmd", on_command(outside));
    t.on("pane", "pane_cmd", on_pane());
    t.on(PRESS, "pane_cmd", on_press());
}

pub fn register(t: &mut Table) {
    register_with(t, real());
}

pub fn register_late_with(t: &mut Table, outside: Arc<dyn Outside>) {
    t.on("start", "pane_cmd", on_start_late(outside));
}

/// The entries that run after every other feature's `start` (`app::table`).
pub fn register_late(t: &mut Table) {
    register_late_with(t, real());
}
