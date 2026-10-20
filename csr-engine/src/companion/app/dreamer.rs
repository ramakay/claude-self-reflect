//! The removed `dreamer.ts`: looks and dreams, the side-question fork, answers and the dreams seen.
//! Port of the removed `dreamer.ts` and of the dreamer's parts of the TypeScript companion.
//!
//! The entries, where the TypeScript runs each group:
//! - `step` (Stream `/after`): a step the pace counted, once the chapter's step is due,
//!   starts a look (one at a time, whoever started it); any other counted step lets the fleet offer
//!   the reports that wait.
//! - `fork` (Gate): You should know's forks get CSR's facts and a `ysk` record; any
//!   other plugin's fork is noted and passes.
//! - `chapter` (Gate): a dream still open at a compaction is carried once, then expires. The next
//!   chapter itself is pace's.
//! - `digit` (Gate): a dream answered in the pane, a dream on its way to Claude, and
//!   the person's last three prompts.
//! - `start`: the fleet's `Dream` seam, and the 15 s look at the dream log.
//!
//! What the TypeScript ran as processes (the removed `lease.py`, the removed `pane.py`, `git`) is
//! `outside::Outside`, so the engine never spawns itself and a test puts a stand-in in its place.

mod answers;
mod look;
pub mod outside;
mod prompt;
#[cfg(test)]
mod tests;
pub mod text;

pub use self::answers::{answered, sent_to_claude, settled_in_the_pane, typed_past};
use self::{
    look::{dream, Trigger},
    outside::{Here, Outside},
    text::{
        cited_from, is_ysk_origin, is_ysk_propose, own_prompt, with_csr_facts, ysk_facts, ysk_line,
    },
};
use super::{
    fleet::{self, arm_completions, completions, DreamCall, Fleet},
    pace::{counted_chapter, fresh, install_keep, tokens_of},
    state::{config_rows, is_person, update, Options},
};
use crate::companion::{
    relay::{
        dispatch::{handler, Ctx, Handler, Reply, Table},
        host::BoxFuture,
    },
    store::MACHINE,
};
use anyhow::Result;
use serde_json::{json, Value};
use std::{collections::HashMap, future::Future, sync::Arc};

/// A look at the dream log, to see what the pane's buttons answered.
const SETTLED_IN_PANE_MS: f64 = 15_000.;
/// One brief per session per 5 minutes for You should know: it forks at turn steps 6, 12, ... of
/// a long turn.
const YSK_BRIEF_MS: f64 = 5. * 60. * 1000.;
/// The prompts the dreamer is told are already on the table.
const ASKED_KEPT: usize = 3;

/// What the dreamer keeps for the life of the process (`cx.kept`).
#[derive(Default)]
pub struct Dreamer {
    out: Option<Arc<dyn Outside>>,
    /// You should know's last brief of each session: when it was made, and its facts.
    briefs: HashMap<String, (f64, String)>,
}

// ---------------------------------------------------------------- small readers

/// A whole number as JSON the way JavaScript writes one.
pub(crate) fn int(value: f64) -> Value {
    if value.fract() == 0. && value.abs() < 9e15 {
        json!(value as i64)
    } else {
        json!(value)
    }
}

/// The session's row `key` as a number: anything else is 0 (`count`).
pub(crate) fn count(cx: &Ctx, key: &str) -> f64 {
    match cx.store.get(&cx.sid, key) {
        Ok(Some(Value::Number(n))) => n.as_f64().filter(|n| n.is_finite()).unwrap_or(0.),
        _ => 0.,
    }
}

/// The context for the rows that belong to no session.
pub(crate) fn machine(cx: &Ctx) -> Ctx {
    Ctx {
        sid: MACHINE.to_string(),
        ..cx.clone()
    }
}

/// The machine around the engine: this process's own, or the one a test put there.
pub(crate) fn out(cx: &Ctx) -> Arc<dyn Outside> {
    let held = cx.kept(|d: &mut Dreamer| d.out.clone());
    held.unwrap_or_else(|| Arc::new(Here))
}

/// Every entry makes the machine its own, and the pace's `--keep` the same one's.
fn install(cx: &Ctx, outside: &Arc<dyn Outside>) {
    cx.kept(|d: &mut Dreamer| d.out = Some(outside.clone()));
    install_keep(cx, &outside.keep());
}

/// Work that runs apart from the request that started it.
fn detach<F, Fut>(cx: &Ctx, work: F)
where
    F: FnOnce(Ctx) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let cx = cx.clone();
    let host = cx.host.clone();
    host.detach(Box::new(move |host| Box::pin(work(Ctx { host, ..cx }))));
}

async fn dreams_on(cx: &Ctx) -> bool {
    Options::of(&config_rows(cx).await).dreams
}

// ---------------------------------------------------------------- step

/// The fleet's seam: a look for the reports of agents that finished.
pub(crate) fn seam(outside: Arc<dyn Outside>) -> fleet::Dream {
    Arc::new(move |cx: Ctx, call: DreamCall| -> BoxFuture<Result<()>> {
        let outside = outside.clone();
        Box::pin(async move {
            // A timer can be the first to reach the look, before any entry of ours has run.
            install(&cx, &outside);
            let trigger = Trigger::Agent(call.completions);
            dream(&cx, call.step as f64, call.steps as f64, None, trigger).await
        })
    })
}

/// `step`: `told` is `{ e, result }`. Only a step the pace counted gets
/// here with anything to do.
fn on_step(outside: Arc<dyn Outside>) -> Handler {
    handler(move |cx, told| {
        let outside = outside.clone();
        async move {
            install(&cx, &outside);
            if !fresh(&cx) {
                return Ok(Reply::none());
            }
            let Some(chapter) = counted_chapter(&cx) else {
                return Ok(Reply::none());
            };
            let usage = &told["result"]["usage"];
            let spent = if usage.is_object() {
                tokens_of(usage)
            } else {
                0.
            };
            let model = usage["model"].as_str().map(str::to_string);
            let go = cx.kept(|f: &mut Fleet| {
                let go = chapter.steps >= chapter.due && !f.dreaming;
                f.dreaming |= go;
                go
            });
            if go {
                detach(&cx, move |cx| async move {
                    let main = Trigger::Main;
                    let _ = dream(&cx, spent, chapter.steps, model, main).await;
                    cx.kept(|f: &mut Fleet| f.dreaming = false);
                    // A report that came in during this look would otherwise wait for an event.
                    arm_completions(&cx, None, false).await;
                });
            } else {
                // The next step of the main session: a finished agent's turn.
                detach(&cx, |cx| async move { completions(&cx).await });
            }
            Ok(Reply::none())
        }
    })
}

// ---------------------------------------------------------------- fork

/// You should know's brief of this session, at most one in 5 minutes: its code rule, correction
/// and past-session lines.
async fn ysk_facts_for(cx: &Ctx) -> String {
    let now = cx.clock.now();
    let held = cx.kept(|d: &mut Dreamer| d.briefs.get(&cx.sid).cloned());
    if let Some((at, facts)) = held {
        if now - at < YSK_BRIEF_MS {
            return facts;
        }
    }
    let brief = out(cx)
        .brief(&cx.cwd, &cx.sid, now)
        .await
        .unwrap_or_default();
    let facts = ysk_facts(&brief);
    cx.kept(|d: &mut Dreamer| d.briefs.insert(cx.sid.clone(), (now, facts.clone())));
    facts
}

/// `model.fork`, before it runs: what only the event knows.
fn on_fork_event(outside: Arc<dyn Outside>) -> Handler {
    handler(move |cx, e| {
        let outside = outside.clone();
        async move {
            install(&cx, &outside);
            if !is_person(&cx).await {
                return Ok(Reply::none());
            }
            let named = |name: &str| e["origin"][name].as_str().map(str::to_string);
            // The relay forwards the fork's origin when it knows it; with none, it is no seat of
            // You should know's.
            let plugin = named("plugin");
            let origin = match (&plugin, named("tier")) {
                (Some(plugin), Some(tier)) => format!("{plugin}/{tier}"),
                (Some(plugin), None) => format!("{plugin}/undefined"),
                _ => "unknown".to_string(),
            };
            if !plugin.as_deref().is_some_and(is_ysk_origin) {
                // Another plugin's fork: keep its seat name, so a You should know look that names
                // itself some other way shows up in the log instead of passing unseen.
                let record = json!({ "kind": "ysk", "at": int(cx.clock.now() / 1000.),
                    "session": cx.sid, "outcome": "other-origin", "origin": origin });
                detach(&cx, move |cx| async move {
                    (out(&cx).keep())(cx.cwd.clone(), record).await;
                });
                return Ok(Reply::none());
            }
            let began = cx.clock.now();
            let prompt = e["prompt"].as_str().unwrap_or("").to_string();
            let propose = is_ysk_propose(&prompt);
            let given = if propose && !out(&cx).no_ysk_brief() {
                ysk_facts_for(&cx).await
            } else {
                String::new()
            };
            let left = json!({ "ysk": true, "origin": origin, "began": int(began),
                "propose": propose, "given": given });
            let mut reply = Reply {
                left: Some(left),
                ..Reply::none()
            };
            if !given.is_empty() {
                let mut sent = e.clone();
                sent["prompt"] = json!(with_csr_facts(&prompt, &given));
                reply.next = Some(sent);
            }
            Ok(reply)
        }
    })
}

/// `model.fork`, after it ran: `result` is `{ value }`, or `{ deny }` when it threw. The
/// `ysk` record of what came of the look.
fn on_fork_after() -> Handler {
    handler(|cx, result| async move {
        let Some(left) = cx.left.clone().filter(|left| left["ysk"] == true) else {
            return Ok(Reply::none());
        };
        let reply = Some(&result["value"]).filter(|reply| reply.is_object());
        let propose = left["propose"] == true;
        let given = left["given"].as_str().unwrap_or("");
        let answered_text = match reply {
            Some(reply) if reply["isAnswered"] == true => {
                Some(reply["text"].as_str().unwrap_or(""))
            }
            _ => None,
        };
        let said = answered_text.map(ysk_line);
        let outcome = match (&reply, &said) {
            (None, _) => Some("denied".to_string()),
            (Some(reply), None) => reply["reason"].as_str().map(str::to_string),
            (Some(_), Some(_)) if !propose => Some("explained".to_string()),
            (Some(_), Some((Some(_), _))) => Some("line".to_string()),
            (Some(_), Some((None, true))) => Some("none".to_string()),
            (Some(_), Some(_)) => Some("unparsed".to_string()),
        };
        let cited = match (answered_text, given.is_empty()) {
            (Some(text), false) => cited_from(text, given),
            _ => Vec::new(),
        };
        let began = left["began"].as_f64().unwrap_or(0.);
        let mut record = json!({
            "kind": "ysk", "at": int(cx.clock.now() / 1000.), "session": cx.sid,
            "origin": left["origin"],
            "prompt": if propose { "propose" } else { "explain" },
            "line": said.as_ref().and_then(|(line, _)| line.clone()),
            "given": given.encode_utf16().count(), "cited": cited,
            "took_ms": int(cx.clock.now() - began),
        });
        if let Some(outcome) = outcome {
            record["outcome"] = json!(outcome);
        }
        if let Some(usage) = reply
            .map(|reply| &reply["usage"])
            .filter(|usage| !usage.is_null())
        {
            record["usage"] = usage.clone();
        }
        let keep = out(&cx).keep();
        detach(
            &cx,
            move |cx| async move { keep(cx.cwd.clone(), record).await },
        );
        Ok(Reply::none())
    })
}

// ---------------------------------------------------------------- chapter

/// `chapter`, before the compaction: what only the event knows.
fn on_compact_event() -> Handler {
    handler(|_cx, e| async move {
        let skip = !e["agentId"].is_null() || e["trigger"] == "precompute";
        Ok(Reply {
            left: Some(json!({ "skip": skip })),
            ..Reply::none()
        })
    })
}

/// `chapter`, after it: a dream still open is carried into the next
/// chapter once, marked "still open", with two prompts again to answer it; one already carried
/// expires. The chapter itself starts in pace's entry, which ran first. A `skip` (a hook vetoed it)
/// or a `deny` (the call threw) in the result is no compaction.
fn on_compact_after(outside: Arc<dyn Outside>) -> Handler {
    handler(move |cx, result| {
        let outside = outside.clone();
        async move {
            install(&cx, &outside);
            let skip = cx.left.as_ref().is_some_and(|left| left["skip"] == true);
            let over = !result["skip"].is_null() || result.get("deny").is_some();
            if skip || over || !dreams_on(&cx).await {
                return Ok(Reply::none());
            }
            if !is_person(&cx).await {
                return Ok(Reply::none());
            }
            let (mut expired, mut carried) = (None, None);
            update(&cx, "view", |current| match current {
                Some(view) if view["kind"] != "asked" && view["note"]["kind"] == "dream" => {
                    if view["carried"].as_bool().unwrap_or(false) {
                        expired = Some(view["note"].clone());
                        return None;
                    }
                    carried = Some(view["note"].clone());
                    let mut view = view;
                    view["carried"] = json!(true);
                    view["promptsSurvived"] = json!(0);
                    Some(view)
                }
                other => other,
            })?;
            if let Some(note) = expired {
                answered(&cx, &note, "expired", false, None).await?;
            }
            if let Some(note) = carried {
                let at = cx.clock.now();
                let record = json!({ "kind": "answer", "id": note["id"], "answer": "carried",
                    "at": int(at / 1000.) });
                (out(&cx).keep())(cx.cwd.clone(), record).await;
            }
            Ok(Reply::none())
        }
    })
}

// ---------------------------------------------------------------- digit

/// `prompt.submit`: a dream answered in the pane is never counted as
/// typed past; a dream on its way to Claude is logged; the person's last three prompts are kept.
fn on_digit(outside: Arc<dyn Outside>) -> Handler {
    handler(move |cx, e| {
        let outside = outside.clone();
        async move {
            install(&cx, &outside);
            let kind = e["origin"]["kind"].as_str();
            let yours = matches!(kind, Some("composer" | "bridge"));
            let text = e["text"].as_str().unwrap_or("");
            let _ = settled_in_the_pane(&cx).await;
            let on = dreams_on(&cx).await;
            if on {
                let _ = sent_to_claude(&cx, text, yours).await;
            }
            // The person's last three prompts go to the dreamer as already on the table. Kept per
            // session; a forwarded note or dream is not the person's ask.
            let own = if on && yours { own_prompt(text) } else { None };
            if let Some(own) = own {
                let mut asked = crate::companion::app::state::strings(&cx, &cx.sid, "asked");
                asked.push(own);
                let from = asked.len().saturating_sub(ASKED_KEPT);
                let _ = cx.store.set(&cx.sid, "asked", &json!(asked[from..]));
            }
            Ok(Reply::none())
        }
    })
}

// ---------------------------------------------------------------- start

/// `start`: the fleet's seam, then the look at the dream log every 15 s.
fn on_start(outside: Arc<dyn Outside>) -> Handler {
    handler(move |cx, e| {
        let outside = outside.clone();
        async move {
            install(&cx, &outside);
            cx.kept(|f: &mut Fleet| f.dream = Some(seam(outside.clone())));
            if e["isInteractive"] != true {
                return Ok(Reply::none());
            }
            cx.every(SETTLED_IN_PANE_MS, |cx| async move {
                let _ = settled_in_the_pane(&cx).await;
            });
            Ok(Reply::none())
        }
    })
}

pub fn register_with(t: &mut Table, outside: Arc<dyn Outside>) {
    t.after("step", "dreamer", on_step(outside.clone()));
    t.gate(
        "fork",
        "dreamer",
        on_fork_event(outside.clone()),
        on_fork_after(),
    );
    t.gate(
        "chapter",
        "dreamer",
        on_compact_event(),
        on_compact_after(outside.clone()),
    );
    t.on("digit", "dreamer", on_digit(outside.clone()));
    t.on("start", "dreamer", on_start(outside));
}

pub fn register(t: &mut Table) {
    register_with(t, Arc::new(Here));
}
