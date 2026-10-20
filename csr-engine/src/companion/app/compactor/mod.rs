//! Compact at the right moment: port of the compactor of `hooks/register.tsx` (`CS`, `atTurnEnd`,
//! `compactNow`, the countdown, the compaction marker and the band's data) and of
//! `hooks/compact.ts` (the rules, in `rules`).
//!
//! The entries, on the keys `app::table()` names: `stop`, `watched`, `seen`, `turn_start`,
//! `turn_complete`, `edit`, `marker`, `config` and `start`. The TypeScript registers the first
//! three only when `autoCompact` is not `off`, and reloads when the setting changes; the engine
//! lives on, so each of them asks the setting when it runs (`config.list`, once, then what a
//! `/config` write says).
//!
//! What the TypeScript did through `process.run`, `fs` and `env` the engine does itself, behind
//! `Outside`, which a test replaces.
//!
//! The band's two buttons are pressed through the `press` key, which `setup_walk` owns in the
//! table: `on_press` is what the band's code calls for `compact-now` and `compact-cancel`.

mod flow;
pub mod outside;
pub mod rules;
#[cfg(test)]
mod tests;
mod watch;

use crate::companion::{
    app::state::{is_person, update, Options},
    relay::dispatch::{handler, Ctx, Reply, Table},
};
use anyhow::Result;
use flow::{cancel_compact, Out};
use outside::{Here, Redact};
use rules::{
    countdown_text, num, offer_text, rule_in, ser_num, ser_opt_num, Human, Mode, Payback, Running,
    Seam, Setting, ShownView, Source, ViewKind,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
};

/// The owner's last rules the summary keeps.
pub const RULES_KEPT: usize = 3;
const BACKGROUND: [&str; 2] = ["Agent", "Workflow"];
const EDITS: [&str; 3] = ["Edit", "Write", "NotebookEdit"];
const MARKERS: &str = ".claude-self-reflect/csr-companion/compacting";

/// What a countdown or an offer would compact with. It rides in the band's view, so a reload
/// can resume it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Armed {
    #[serde(serialize_with = "ser_num")]
    pub ctx: f64,
    pub payback: Option<Payback>,
    pub seam: Seam,
    pub mode: Mode,
    pub forced: bool,
    #[serde(default, serialize_with = "ser_opt_num")]
    pub floor: Option<f64>,
    pub answer: String,
}

/// Background work as the last main Stop listed it.
#[derive(Clone, Debug, Default)]
pub struct Stopped {
    pub at: f64,
    pub running: Vec<Running>,
}

/// One process, one module, one session: what the hooks learn between turn ends (the TypeScript's
/// `CS`).
#[derive(Default)]
pub struct Compactor {
    pub(crate) setting: Setting,
    /// Whether `setting` is what `/config` holds, or only the default until it can be read.
    pub(crate) loaded: bool,
    /// One more when an operation starts.
    pub(crate) plan_version: u64,
    /// One more at each `/config` change of the setting: an automatic compaction acts on the one
    /// it started under.
    pub(crate) setting_stamp: u64,
    pub(crate) loaded_at: Option<f64>,
    /// The last prompt a person sent: where from, and when.
    pub(crate) last: Option<Human>,
    pub(crate) stopped: Option<Stopped>,
    /// Background work seen launched and not yet reported done, by the call that launched it.
    pub(crate) launched: Vec<(String, Running)>,
    /// When the running turn started.
    pub(crate) turn_at: f64,
    pub(crate) turn_running: bool,
    /// AskUserQuestion calls still open.
    pub(crate) asking: i64,
    pub(crate) edits: u64,
    pub(crate) commit: Option<String>,
    pub(crate) gate: bool,
    /// The context at the owner's last cancel, until the context leaves the band.
    pub(crate) cancelled_at: Option<f64>,
    /// What a countdown or an offer would compact with.
    pub(crate) armed: Option<Arc<Armed>>,
    pub(crate) timers: Vec<u64>,
    pub(crate) compacting: bool,
    /// The owner's binding sentences this session.
    pub(crate) rules: Vec<String>,
    /// The files this session's Edit and Write calls wrote, its subagents' too.
    pub(crate) wrote: BTreeSet<String>,
    /// The folders the main thread committed in.
    pub(crate) committed_in: BTreeSet<String>,
    /// Background tasks a subagent started: task id to the subagent.
    pub theirs: HashMap<String, String>,
    /// Plan writes finish in the order they were asked.
    pub(crate) queue: Arc<tokio::sync::Mutex<()>>,
}

impl Compactor {
    /// Background work in flight: the Stop's own list when it came during this turn, else what
    /// the hooks saw launched and not reported done.
    pub(crate) fn running(&self) -> Vec<Running> {
        match &self.stopped {
            Some(stopped) if stopped.at >= self.turn_at => stopped.running.clone(),
            _ => self.launched.iter().map(|(_, task)| task.clone()).collect(),
        }
    }
}

/// The band's view of a countdown or an offer, as the row `(sid, "compact")` keeps it.
pub(crate) fn view_json(kind: &str, at: &Armed, why: &str, ends_at: Option<f64>) -> Value {
    let mut view = json!({
        "kind": kind, "ctx": num(at.ctx), "why": why,
        "payback": at.payback, "armed": at,
    });
    if let Some(ends_at) = ends_at {
        view["endsAt"] = num(ends_at);
    }
    view
}

pub(crate) fn shown_of(view: &Value) -> ShownView {
    ShownView {
        kind: if view["kind"] == "offer" {
            ViewKind::Offer
        } else {
            ViewKind::Countdown
        },
        ends_at: view["endsAt"].as_f64(),
        has_armed: !view["armed"].is_null() && view["armed"] != json!(false),
    }
}

/// JavaScript's truthiness of a JSON value.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// The setting: what `/config` holds, asked of `config.list` the first time it is needed (and
/// again while that fails).
pub async fn setting(cx: &Ctx) -> Setting {
    let (loaded, held) = cx.kept(|s: &mut Compactor| (s.loaded, s.setting));
    if loaded {
        return held;
    }
    if let Ok(Value::Array(rows)) = cx.host.call("config.list", vec![]).await {
        let asked = Setting::of(&json!(Options::of(&rows).auto_compact));
        cx.kept(|s: &mut Compactor| {
            if !s.loaded {
                s.setting = asked;
                s.loaded = true;
            }
        });
    }
    cx.kept(|s: &mut Compactor| s.setting)
}

fn has_agent(e: &Value) -> bool {
    e.get("agentId").is_some_and(|id| !id.is_null())
}

/// The task owners the session kept, back into memory (a respawned engine starts with none).
fn restore_owners(cx: &Ctx) {
    let Ok(Some(Value::Object(owners))) = cx.store.get(&cx.sid, "taskOwners") else {
        return;
    };
    cx.kept(|s: &mut Compactor| {
        for (id, agent) in owners {
            if let Some(agent) = agent.as_str() {
                s.theirs.insert(id, agent.to_string());
            }
        }
    });
}

fn own_task(cx: &Ctx, id: &str, agent: &str) -> Result<()> {
    cx.kept(|s: &mut Compactor| s.theirs.insert(id.to_string(), agent.to_string()));
    update(cx, "taskOwners", |current| {
        let mut owners = match current {
            Some(Value::Object(owners)) => owners,
            _ => serde_json::Map::new(),
        };
        owners.insert(id.to_string(), json!(agent));
        Some(Value::Object(owners))
    })?;
    Ok(())
}

/// A prompt a person sent, or a background task's report.
async fn prompt_seen(cx: &Ctx, out: &Out, kind: &str, text: &str) -> Result<()> {
    let now = cx.clock.now();
    cx.kept(|s: &mut Compactor| {
        s.loaded_at.get_or_insert(now);
    });
    if kind == "composer" || kind == "bridge" {
        let at = cx.clock.now();
        let source = if kind == "composer" {
            Source::Composer
        } else {
            Source::Bridge
        };
        let armed = cx.kept(|s: &mut Compactor| {
            s.last = Some(Human { kind: source, at });
            if let Some(rule) = rule_in(text) {
                if !s.rules.contains(&rule) {
                    s.rules.push(rule);
                }
            }
            s.armed.is_some()
        });
        if armed {
            // a prompt cancels a countdown, and puts an offer away
            cancel_compact(cx, out, "prompt").await?;
        }
        return Ok(());
    }
    // A background task reported: done when it says so, and what it reports counts for this
    // turn's end.
    let group = |re: &regex::Regex| {
        re.captures(text)
            .and_then(|found| found.get(1))
            .map(|found| found.as_str().to_string())
    };
    let done = watch::TN_DONE.is_match(text);
    let (tool, task) = (group(&watch::TN_TOOL), group(&watch::TN_TASK));
    let found = rules::commit_in(text);
    let gate = rules::gate_in(text);
    cx.kept(|s: &mut Compactor| {
        if done {
            s.launched.retain(|(id, item)| {
                !(tool.as_deref() == Some(id.as_str()) || task.as_deref() == Some(item.id.as_str()))
            });
        }
        s.commit = found.or(s.commit.take());
        s.gate = s.gate || gate;
    });
    Ok(())
}

/// What a tool call left in `/event` for its `/after`.
fn watched_event(cx: &Ctx, e: &Value) -> Reply {
    let tool = e["tool"].as_str().unwrap_or("");
    let sub = has_agent(e);
    cx.kept(|s: &mut Compactor| {
        if sub {
            return;
        }
        if tool == "AskUserQuestion" {
            s.asking += 1;
        } else if EDITS.contains(&tool) {
            s.edits += 1;
        }
    });
    let file = ["file_path", "notebook_path"]
        .iter()
        .find_map(|name| e[*name].as_str());
    Reply {
        left: Some(json!({
            "tool": tool, "file": file, "command": e["command"], "id": e["tool_use_id"],
            "background": e["run_in_background"], "description": e["description"],
            "agent": e["agentId"],
        })),
        ..Reply::none()
    }
}

/// The tool call has answered: what it wrote, committed or started.
async fn watched_after(cx: &Ctx, result: &Value) -> Result<Reply> {
    let Some(left) = cx.left.as_ref() else {
        return Ok(Reply::none());
    };
    let tool = left["tool"].as_str().unwrap_or("");
    let agent = left["agent"].as_str();
    let sub = !left["agent"].is_null();
    let failed = truthy(&result["isError"]) || result.get("deny").is_some();
    if tool == "AskUserQuestion" && !sub {
        cx.kept(|s: &mut Compactor| s.asking -= 1);
        return Ok(Reply::none());
    }
    if let (true, Some(file)) = (
        EDITS.contains(&tool),
        left["file"].as_str().filter(|f| !f.is_empty()),
    ) {
        if !failed {
            let path = if file.starts_with('/') {
                file.to_string()
            } else {
                format!("{}/{file}", cx.cwd)
            };
            cx.kept(|s: &mut Compactor| s.wrote.insert(path));
        }
    }
    let text = result["text"].as_str().unwrap_or("");
    if sub {
        let found = watch::TASK_ID.captures(text).and_then(|found| found.get(1));
        let watched = BACKGROUND.contains(&tool) || tool == "Bash" || tool == "Monitor";
        if let (Some(id), true, false, Some(agent)) = (found, watched, failed, agent) {
            own_task(cx, id.as_str(), agent)?;
        }
        return Ok(Reply::none());
    }
    let command = left["command"].as_str();
    if let (true, false, Some(command)) = (tool == "Bash", failed, command) {
        if watch::GIT_COMMIT.is_match(command) {
            let sha = watch::COMMIT_OUT
                .captures(text)
                .and_then(|found| found.get(1));
            if let Some(sha) = sha {
                let short: String = sha.as_str().chars().take(7).collect();
                let dir = watch::commit_dir(command, &cx.cwd);
                cx.kept(|s: &mut Compactor| {
                    s.commit = Some(short);
                    s.committed_in.extend(dir);
                });
            }
        }
    }
    let background = if BACKGROUND.contains(&tool) {
        watch::LAUNCHED.is_match(text)
    } else {
        tool == "Bash" && left["background"] == json!(true) && !truthy(&result["isError"])
    };
    if background {
        let kind = match tool {
            "Bash" => "shell",
            "Workflow" => "workflow",
            _ => "subagent",
        };
        let call = left["id"].as_str().unwrap_or("").to_string();
        let id = (watch::TASK_ID.captures(text).and_then(|found| found.get(1)))
            .map_or_else(|| call.clone(), |found| found.as_str().to_string());
        let description = left["description"].as_str().unwrap_or("");
        let task = Running::new(kind, &id, description);
        cx.kept(
            |s: &mut Compactor| match s.launched.iter_mut().find(|(key, _)| *key == call) {
                Some((_, was)) => *was = task,
                None => s.launched.push((call, task)),
            },
        );
    }
    Ok(Reply::none())
}

/// While a compaction runs, a marker file says so and when it began; the pane draws the timer
/// from it. It is removed when the compaction ends, however it ends. Nothing here can fail the
/// compaction.
async fn mark_compacting(cx: &Ctx, out: &Out, trigger: &Value) -> Option<String> {
    let home = out.home().await?;
    let path = format!("{home}/{MARKERS}/{}.json", cx.sid);
    let body = json!({ "at": num(cx.clock.now() / 1000.), "trigger": trigger }).to_string();
    out.write(&path, &body, Redact::Nothing).await.ok()?;
    Some(path)
}

/// Removed with rm; where that cannot run, the marker is overwritten as ended, which the pane
/// reads as none.
async fn unmark_compacting(out: &Out, path: &str) {
    out.remove(path).await;
    if out.exists(path).await {
        let _ = out
            .write(path, &json!({ "ended": true }).to_string(), Redact::Nothing)
            .await;
    }
}

/// The entries, with the outside they use.
pub fn register_with(t: &mut Table, out: Out) {
    t.on(
        "stop",
        "compactor",
        handler(move |cx, e| async move {
            if setting(&cx).await == Setting::Off {
                return Ok(Reply::none());
            }
            restore_owners(&cx);
            let tasks = e["background_tasks"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let stopped = flow::stopped(cx.clock.now(), &tasks);
            cx.kept(|s: &mut Compactor| s.stopped = Some(stopped));
            Ok(Reply::none())
        }),
    );
    t.gate(
        "watched",
        "compactor",
        handler(move |cx, e| async move {
            if setting(&cx).await == Setting::Off {
                return Ok(Reply::none());
            }
            Ok(watched_event(&cx, &e))
        }),
        handler(move |cx, result| async move { watched_after(&cx, &result).await }),
    );
    let o = out.clone();
    t.on(
        "seen",
        "compactor",
        handler(move |cx, e| {
            let out = o.clone();
            async move {
                if setting(&cx).await == Setting::Off {
                    return Ok(Reply::none());
                }
                let kind = e["origin"]["kind"].as_str().unwrap_or("");
                if matches!(kind, "composer" | "bridge" | "task-notification") {
                    prompt_seen(&cx, &out, kind, e["text"].as_str().unwrap_or("")).await?;
                }
                Ok(Reply::none())
            }
        }),
    );
    let o = out.clone();
    t.on(
        "turn_start",
        "compactor",
        handler(move |cx, _e| {
            let out = o.clone();
            async move {
                if setting(&cx).await != Setting::Off {
                    flow::turn_started(&cx, &out).await?;
                }
                Ok(Reply::none())
            }
        }),
    );
    let o = out.clone();
    t.on(
        "turn_complete",
        "compactor",
        handler(move |cx, e| {
            let out = o.clone();
            async move {
                if has_agent(&e) || setting(&cx).await == Setting::Off {
                    return Ok(Reply::none());
                }
                cx.kept(|s: &mut Compactor| s.turn_running = false);
                let answered = e["reason"] == "answer";
                let compacting = cx.kept(|s: &mut Compactor| s.compacting);
                if answered && !compacting && is_person(&cx).await {
                    let _ = flow::at_turn_end(&cx, &out, e["answer"].as_str().unwrap_or("")).await;
                }
                cx.kept(|s: &mut Compactor| {
                    s.edits = 0;
                    s.commit = None;
                    s.gate = false;
                });
                Ok(Reply::none())
            }
        }),
    );
    let o = out.clone();
    t.on(
        "edit",
        "compactor",
        handler(move |cx, e| {
            let out = o.clone();
            async move {
                // A lone digit into an empty box is a band hotkey on its way, not typing.
                let key = e["key"]["key"].as_str().unwrap_or("");
                let digit = key.len() == 1 && key.as_bytes()[0].is_ascii_digit();
                if e["text"] == "" && digit {
                    return Ok(Reply::none());
                }
                let attended = cx.kept(|s: &mut Compactor| {
                    s.armed
                        .as_ref()
                        .is_some_and(|armed| armed.mode == Mode::Attended)
                });
                if attended {
                    cancel_compact(&cx, &out, "typing").await?;
                }
                Ok(Reply::none())
            }
        }),
    );
    let (o, p) = (out.clone(), out.clone());
    t.gate(
        "marker",
        "compactor",
        handler(move |cx, e| {
            let out = o.clone();
            async move {
                if has_agent(&e) {
                    return Ok(Reply::none());
                }
                let left = mark_compacting(&cx, &out, &e["trigger"])
                    .await
                    .map(Value::from);
                Ok(Reply {
                    left,
                    ..Reply::none()
                })
            }
        }),
        handler(move |cx, _result| {
            let out = p.clone();
            async move {
                if let Some(path) = cx.left.as_ref().and_then(Value::as_str) {
                    unmark_compacting(&out, path).await;
                }
                Ok(Reply::none())
            }
        }),
    );
    let o = out.clone();
    t.gate(
        "config",
        "compactor",
        handler(move |_cx, e| async move {
            let key = e["key"].as_str().unwrap_or("");
            if !watch::AUTO_ROW.is_match(key) {
                return Ok(Reply::none());
            }
            Ok(Reply {
                left: Some(json!({ "value": e["value"] })),
                ..Reply::none()
            })
        }),
        handler(move |cx, done| {
            let out = o.clone();
            async move {
                let Some(left) = cx.left.as_ref() else {
                    return Ok(Reply::none());
                };
                if !truthy(&done["deny"]) {
                    flow::setting_changed(&cx, &out, Setting::of(&left["value"])).await?;
                }
                Ok(Reply::none())
            }
        }),
    );
    t.on(
        "start",
        "compactor",
        handler(move |cx, e| {
            let out = out.clone();
            async move {
                if e["isInteractive"] != json!(true) {
                    return Ok(Reply::none());
                }
                // a reload mid-countdown: it runs on, never stuck at 0s. With nothing in the band
                // there is nothing to resume, and the setting is not asked for.
                let shown = cx.store.get(&cx.sid, "compact").ok().flatten();
                if shown.is_some_and(|view| !view.is_null()) {
                    setting(&cx).await;
                    let _ = flow::resume_armed(&cx, &out).await;
                }
                Ok(Reply::none())
            }
        }),
    );
}

pub fn register(t: &mut Table) {
    register_with(t, Arc::new(Here::new()));
}

/// The band while CSR counts down or offers; none when it does neither. `quiet`: no hotkeys of
/// its own (another band that asks is under it).
pub async fn compact_band_quiet(cx: &Ctx, quiet: bool) -> Option<Value> {
    let shown = cx
        .store
        .get(&cx.sid, "compact")
        .ok()
        .flatten()
        .filter(|view| !view.is_null())?;
    let ctx = shown["ctx"].as_f64().unwrap_or(0.);
    let why = shown["why"].as_str().unwrap_or("");
    let text = if shown["kind"] == "countdown" {
        let left = shown["endsAt"].as_f64().unwrap_or(0.) - cx.clock.now();
        countdown_text(left, ctx, why)
    } else {
        let payback: Option<Payback> = serde_json::from_value(shown["payback"].clone()).ok();
        offer_text(ctx, payback.as_ref(), why)
    };
    let node = |kind: &str, props: Value, children: Vec<Value>| json!({ "type": kind, "props": props, "children": children });
    let button = |key: &str, hotkey: &str, label: &str, role: Option<&str>| {
        let mut props = json!({ "key": key, "hotkey": hotkey, "plain": true, "label": label });
        if let Some(role) = role {
            props["role"] = json!(role);
        }
        node(
            "Box",
            json!({ "marginRight": 3 }),
            vec![node("Button", props, vec![])],
        )
    };
    let lead = node(
        "Box",
        json!({ "flexDirection": "row", "alignItems": "flex-start" }),
        vec![
            node(
                "Box",
                json!({ "flexShrink": 0, "width": 2 }),
                vec![node(
                    "Text",
                    json!({ "color": "suggestion" }),
                    vec![json!("☾")],
                )],
            ),
            node("Text", json!({ "wrap": "wrap" }), vec![json!(text)]),
        ],
    );
    let below = if quiet {
        node(
            "Box",
            json!({ "marginLeft": 2 }),
            vec![node(
                "Text",
                json!({ "dimColor": true }),
                vec![json!("Type anything to cancel.")],
            )],
        )
    } else {
        let cancel = if shown["kind"] == "offer" {
            "Not now"
        } else {
            "Cancel"
        };
        node(
            "Box",
            json!({ "marginLeft": 2, "flexWrap": "wrap" }),
            vec![
                button("compact-now", "1", "Compact now", None),
                button("compact-cancel", "0", cancel, Some("dismiss")),
            ],
        )
    };
    Some(node(
        "Box",
        json!({ "flexDirection": "column", "marginTop": 1 }),
        vec![lead, below],
    ))
}

/// The band while CSR counts down or offers (compactBand).
pub async fn compact_band(cx: &Ctx) -> Option<Value> {
    compact_band_quiet(cx, false).await
}

/// A press on one of the band's buttons; true when the key was one of these two.
pub async fn on_press_with(cx: &Ctx, out: &Out, key: &str) -> Result<bool> {
    match key {
        "compact-now" => flow::press_compact(cx, out),
        "compact-cancel" => cancel_compact(cx, out, "press").await?,
        _ => return Ok(false),
    }
    Ok(true)
}

pub async fn on_press(cx: &Ctx, key: &str) -> Result<bool> {
    let out: Out = Arc::new(Here::new());
    on_press_with(cx, &out, key).await
}
