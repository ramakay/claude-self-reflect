//! The plan, the countdown, the offer and the compaction itself: `showPlan` to `turnStarted` of
//! the TypeScript companion. Every step takes the plan operation it began under; a newer one
//! (the setting changed, a cancel) overtakes the older one, which then writes nothing.

use super::{
    outside::{Outside, Redact},
    rules::{
        act_why, countdown_text, decide, keep_list, keep_rows, mode_of, notice_line, num,
        plan_text, seam_of, width, Act, Ask, Pay, Payback, Resume, Seam, Setting, TurnEnd,
        COUNTDOWN_MS, REARM_TOKENS,
    },
    watch::keep_of,
    Armed, Compactor, Stopped,
};
use crate::companion::{
    app::{
        carry::{threads_of, waiting_on, LOST_LIMIT},
        state::{k, update, BAND_LOW},
    },
    relay::dispatch::Ctx,
};
use anyhow::Result;
use serde_json::{json, Map, Value};
use std::sync::Arc;

pub type Out = Arc<dyn Outside>;

/// Under HOME: `<session>.json`, the herdr pane draws it.
pub const PLANS: &str = ".claude-self-reflect/csr-companion/compact-plan";

/// One operation on the plan: the setting's stamp it began under, and its place in the order.
#[derive(Clone, Copy, Debug)]
pub struct PlanOp {
    stamp: u64,
    sequence: u64,
}

pub fn plan_op(cx: &Ctx, stamp: Option<u64>) -> PlanOp {
    cx.kept(|s: &mut Compactor| {
        s.plan_version += 1;
        PlanOp {
            stamp: stamp.unwrap_or(s.setting_stamp),
            sequence: s.plan_version,
        }
    })
}

pub fn current_plan(cx: &Ctx, op: PlanOp) -> bool {
    cx.kept(|s: &mut Compactor| op.stamp == s.setting_stamp && op.sequence == s.plan_version)
}

fn view_of(cx: &Ctx) -> Option<Value> {
    cx.store
        .get(&cx.sid, "compact")
        .ok()
        .flatten()
        .filter(|view| !view.is_null())
}

fn has_plan(cx: &Ctx) -> bool {
    let row = cx.store.get(&cx.sid, "infinite").ok().flatten();
    row.is_some_and(|row| row["plan"].as_str().is_some_and(|text| !text.is_empty()))
}

pub async fn invalidate(cx: &Ctx) {
    let _ = cx.host.call("ui.invalidate", vec![]).await;
}

/// The payback as the log and the plan file keep it, flat: requests, and the session's requests a
/// turn.
fn payback_fields(payback: Option<&Payback>) -> Map<String, Value> {
    let mut fields = Map::new();
    fields.insert(
        "payback".into(),
        payback.map_or(Value::Null, |p| num(p.requests)),
    );
    fields.insert(
        "per_turn".into(),
        payback.and_then(|p| p.per_turn).map_or(Value::Null, num),
    );
    fields
}

fn seam_fields(seam: &Seam) -> Map<String, Value> {
    let mut fields = Map::new();
    fields.insert(
        "seam".into(),
        json!({
            "is": seam.is_seam, "why": seam.why, "question": seam.question,
            "wait": seam.wait, "running": seam.running, "commit": seam.commit,
            "gate": seam.gate.unwrap_or(false), "edits": seam.edits.unwrap_or(0),
        }),
    );
    fields
}

fn fields(parts: Vec<Map<String, Value>>) -> Map<String, Value> {
    parts.into_iter().flatten().collect()
}

fn one(name: &str, value: Value) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert(name.into(), value);
    map
}

/// Every plan, countdown, cancel, compaction and skip, in the dream log as kind "compact": the
/// pane reads it back.
pub async fn log_compact(cx: &Ctx, out: &Out, event: &str, extra: Map<String, Value>) {
    let at = cx.clock.now() / 1000.;
    let setting = cx.kept(|s: &mut Compactor| s.setting);
    let mut record = Map::new();
    record.insert("kind".into(), json!("compact"));
    record.insert("event".into(), json!(event));
    record.insert("at".into(), num(at));
    record.insert("session".into(), json!(cx.sid));
    record.insert("setting".into(), json!(setting.as_str()));
    record.extend(extra);
    out.keep(&cx.cwd, Value::Object(record)).await;
}

/// Only the latest operation publishes. Writes finish in order, including cancellation.
pub async fn show_plan(
    cx: &Ctx,
    out: &Out,
    state: &str,
    text: Option<String>,
    extra: Map<String, Value>,
    op: PlanOp,
) -> Result<()> {
    let home = out.home().await;
    let path = home.map(|home| format!("{home}/{PLANS}/{}.json", cx.sid));
    let at = cx.clock.now() / 1000.;
    let queue = cx.kept(|s: &mut Compactor| s.queue.clone());
    let _turn = queue.lock().await;
    if !current_plan(cx, op) {
        return Ok(());
    }
    update(cx, "infinite", |current| {
        if !current_plan(cx, op) {
            return current;
        }
        let story = current.map_or(Value::Null, |row| row["story"].clone());
        Some(json!({ "plan": text, "story": story }))
    })?;
    // The plan file only feeds the pane: a failed write never stops the plan or a compaction.
    if let Some(path) = path.filter(|_| current_plan(cx, op)) {
        let mut file = Map::new();
        file.insert("at".into(), num(at));
        file.insert("state".into(), json!(state));
        file.insert("text".into(), json!(text));
        file.extend(extra);
        let body = Value::Object(file).to_string();
        let _ = out.write(&path, &body, Redact::Json).await;
    }
    Ok(())
}

fn cancel_timers(cx: &Ctx) {
    let ids = cx.kept(|s: &mut Compactor| std::mem::take(&mut s.timers));
    for id in ids {
        cx.timers.cancel(id);
    }
}

/// A countdown or an offer ends, however it ends: the band clears, the plan stays.
pub async fn disarm(cx: &Ctx, op: Option<PlanOp>) -> Result<()> {
    if op.is_some_and(|op| !current_plan(cx, op)) {
        return Ok(());
    }
    cancel_timers(cx);
    cx.kept(|s: &mut Compactor| s.armed = None);
    update(cx, "compact", |current| {
        if op.is_none_or(|op| current_plan(cx, op)) {
            None
        } else {
            current
        }
    })?;
    invalidate(cx).await;
    Ok(())
}

/// The owner said not now (0, typing, a prompt): the next seam with 20k more context.
pub async fn cancel_compact(cx: &Ctx, out: &Out, by: &str) -> Result<()> {
    let (was, compacting) = cx.kept(|s: &mut Compactor| (s.armed.clone(), s.compacting));
    let Some(was) = was.filter(|_| !compacting) else {
        return Ok(());
    };
    let op = plan_op(cx, None);
    disarm(cx, Some(op)).await?;
    if !current_plan(cx, op) {
        return Ok(());
    }
    cx.kept(|s: &mut Compactor| s.cancelled_at = Some(was.ctx));
    let wait = format!("cancelled · waits for {}", k(was.ctx + REARM_TOKENS));
    let text = plan_text(was.ctx, was.payback.as_ref(), Some(&wait), was.forced);
    show_plan(cx, out, "planned", Some(text), Map::new(), op).await?;
    let record = fields(vec![
        one("by", json!(by)),
        one("mode", json!(was.mode.as_str())),
        one("ctx_before", num(was.ctx)),
        payback_fields(was.payback.as_ref()),
        seam_fields(&was.seam),
    ]);
    log_compact(cx, out, "cancel", record).await;
    Ok(())
}

/// 1 on the band: compact now, between turns.
pub fn press_compact(cx: &Ctx, out: &Out) {
    let (armed, running) = cx.kept(|s: &mut Compactor| (s.armed.clone(), s.turn_running));
    let Some(at) = armed.filter(|_| !running) else {
        return;
    };
    let out = out.clone();
    let sid = cx.clone();
    cx.host.detach(Box::new(move |host| {
        let cx = Ctx { host, ..sid };
        Box::pin(async move {
            let _ = compact_now(&cx, &out, at, "press", None, None).await;
        })
    }));
}

/// How many threads wait on you in Lost and found (the removed `pane.py --lost`, as prep-compact
/// reads it), for the notice: 0 when it does not answer within two seconds.
async fn lost_waiting(cx: &Ctx, out: &Out) -> usize {
    let asked = out.lost(&cx.cwd, &cx.sid);
    match tokio::time::timeout(LOST_LIMIT, asked).await {
        Ok(Some(found)) => threads_of(&found).map_or(0, |threads| waiting_on(&threads)),
        _ => 0,
    }
}

/// A setting change ends the old operation. Its skip is logged without changing a newer plan.
async fn setting_moved(cx: &Ctx, out: &Out, at: &Armed, by: &str, op: PlanOp) -> Result<()> {
    show_plan(cx, out, "none", None, Map::new(), op).await?;
    let setting = cx.kept(|s: &mut Compactor| s.setting);
    let record = fields(vec![
        one("by", json!(by)),
        one(
            "reason",
            json!(format!("the setting changed to {}", setting.as_str())),
        ),
        one("mode", json!(at.mode.as_str())),
        one("forced", json!(at.forced)),
        one("ctx_before", num(at.ctx)),
        payback_fields(at.payback.as_ref()),
        seam_fields(&at.seam),
    ]);
    log_compact(cx, out, "skip", record).await;
    Ok(())
}

/// One compaction by CSR: the plan row says so first, the summary gets the keep list, the phone
/// gets one line. `stamp`: an automatic one's (unattended, a countdown run out), the setting's
/// stamp when it started. The person may turn Auto off while it is prepared: Auto, unchanged
/// since, is asked for again right before the compaction, and anything else ends it unrun.
pub async fn compact_now(
    cx: &Ctx,
    out: &Out,
    at: Arc<Armed>,
    by: &str,
    stamp: Option<u64>,
    operation: Option<PlanOp>,
) -> Result<()> {
    if cx.kept(|s: &mut Compactor| s.compacting) {
        return Ok(());
    }
    let op = operation.unwrap_or_else(|| plan_op(cx, stamp));
    let still_auto = || {
        current_plan(cx, op)
            && cx.kept(|s: &mut Compactor| {
                stamp.is_none_or(|stamp| s.setting == Setting::Auto && s.setting_stamp == stamp)
            })
    };
    if !still_auto() {
        return setting_moved(cx, out, &at, by, op).await;
    }
    cx.kept(|s: &mut Compactor| s.compacting = true);
    let done = compaction(cx, out, &at, by, op, &still_auto).await;
    cx.kept(|s: &mut Compactor| s.compacting = false);
    done
}

async fn compaction(
    cx: &Ctx,
    out: &Out,
    at: &Armed,
    by: &str,
    op: PlanOp,
    still_auto: &impl Fn() -> bool,
) -> Result<()> {
    disarm(cx, Some(op)).await?;
    let began = cx.clock.now();
    let why = act_why(&at.seam);
    let text = format!("Compacting · {} · {why}", k(at.ctx));
    show_plan(cx, out, "compacting", Some(text), Map::new(), op).await?;
    let keep = keep_of(cx, out.as_ref(), &at.answer).await;
    let instructions = keep_list(&keep);
    // The pane's card streams from these: what it keeps while the summary runs, then the result.
    let record = fields(vec![
        one("by", json!(by)),
        one("mode", json!(at.mode.as_str())),
        one("forced", json!(at.forced)),
        one("ctx_before", num(at.ctx)),
        payback_fields(at.payback.as_ref()),
        one("why", json!(why)),
        one("keep", json!(keep_rows(&keep))),
    ]);
    log_compact(cx, out, "start", record).await;
    if !still_auto() {
        return setting_moved(cx, out, at, by, op).await;
    }
    let mut outcome = "compacted";
    let mut reason: Option<String> = None;
    let mut after: Option<f64> = None;
    match cx
        .host
        .call(
            "session.compact",
            vec![json!({ "instructions": instructions })],
        )
        .await
    {
        Ok(result) => match result.get("skip") {
            Some(skip) => {
                outcome = "skipped";
                reason = Some(
                    skip.as_str()
                        .map_or_else(|| skip.to_string(), str::to_string),
                );
            }
            None => after = result["tokensAfter"].as_f64(),
        },
        Err(error) => {
            // a turn ran, or the summary failed
            outcome = "failed";
            let said = error.to_string();
            reason = Some(
                said.strip_prefix("session.compact: ")
                    .unwrap_or(&said)
                    .to_string(),
            );
        }
    }
    let kept = json!({
        "running": keep.running.iter().map(|task| task.id.clone()).collect::<Vec<_>>(),
        "decisions": keep.decisions.len(), "work": keep.work.len(), "rules": keep.rules.len(),
    });
    let took = cx.clock.now() - began;
    let compacted = outcome == "compacted";
    // read after the summary that decides lost or open
    let waiting = if compacted {
        lost_waiting(cx, out).await
    } else {
        0
    };
    let mut parts = vec![
        one("by", json!(by)),
        one("mode", json!(at.mode.as_str())),
        one("forced", json!(at.forced)),
        one("outcome", json!(outcome)),
        one("reason", json!(reason)),
        one("ctx_before", num(at.ctx)),
        one("ctx_after", after.map_or(Value::Null, num)),
        one("floor", at.floor.map_or(Value::Null, num)),
        payback_fields(at.payback.as_ref()),
        one("took_ms", num(took)),
        one("instructions", json!(width(&instructions))),
        one("kept", kept),
        one("why", json!(why)),
        one("keep", json!(keep_rows(&keep))),
        seam_fields(&at.seam),
    ];
    if compacted {
        parts.push(one("lost_waiting", json!(waiting)));
    }
    log_compact(
        cx,
        out,
        if compacted { "compact" } else { "skip" },
        fields(parts),
    )
    .await;
    if !compacted {
        let said = format!(
            "the compaction did not run ({})",
            reason.unwrap_or_default()
        );
        let text = plan_text(at.ctx, at.payback.as_ref(), Some(&said), at.forced);
        return show_plan(cx, out, "planned", Some(text), Map::new(), op).await;
    }
    cx.kept(|s: &mut Compactor| s.cancelled_at = None);
    show_plan(cx, out, "none", None, Map::new(), op).await?;
    // One line the phone shows: a notice, which the model never reads; its header keeps it out of
    // CSR's memory.
    let line = notice_line(
        at.ctx,
        at.floor.or(after),
        at.payback.as_ref(),
        &keep,
        waiting,
    );
    let row =
        json!({ "message": { "type": "system", "content": [{ "type": "text", "text": line }] } });
    let posted = cx.host.call("session.append", vec![row]).await.ok();
    if posted.is_none_or(|posted| posted.is_null() || posted.get("deny").is_some()) {
        let _ = cx.host.call("ui.log", vec![json!(line)]).await;
    }
    Ok(())
}

/// The countdown's timers: the compaction when it runs out, and the band's seconds redrawn.
pub fn arm_timers(cx: &Ctx, out: &Out, at: &Arc<Armed>, ms: f64) {
    let (at, out) = (at.clone(), out.clone());
    let compact = cx.after(ms, move |cx| {
        let (at, out) = (at.clone(), out.clone());
        async move {
            let (same, auto, stamp) = cx.kept(|s: &mut Compactor| {
                let same = s
                    .armed
                    .as_ref()
                    .is_some_and(|armed| Arc::ptr_eq(armed, &at));
                (same, s.setting == Setting::Auto, s.setting_stamp)
            });
            if same && auto {
                // auto alone
                let _ = compact_now(&cx, &out, at, "countdown", Some(stamp), None).await;
            }
        }
    });
    let redraw = cx.every(1000., |cx| async move { invalidate(&cx).await });
    cx.kept(|s: &mut Compactor| s.timers = vec![compact, redraw]);
}

/// A countdown the setting no longer allows becomes an offer: its timers stop, and a press
/// compacts.
pub async fn offer_instead(
    cx: &Ctx,
    out: &Out,
    at: &Arc<Armed>,
    why: &str,
    op: PlanOp,
) -> Result<()> {
    if !current_plan(cx, op) {
        return Ok(());
    }
    cancel_timers(cx);
    update(cx, "compact", |current| {
        if current_plan(cx, op) {
            Some(super::view_json("offer", at, why, None))
        } else {
            current
        }
    })?;
    let text = plan_text(at.ctx, at.payback.as_ref(), Some("your press"), at.forced);
    let extra = fields(vec![
        one("ctx", num(at.ctx)),
        payback_fields(at.payback.as_ref()),
    ]);
    show_plan(cx, out, "planned", Some(text), extra, op).await
}

/// After a reload: the countdown or the offer the module before this one left in the band.
pub async fn resume_armed(cx: &Ctx, out: &Out) -> Result<()> {
    if cx.kept(|s: &mut Compactor| s.armed.is_some()) {
        return Ok(());
    }
    let op = plan_op(cx, None);
    let shown = view_of(cx);
    let setting = cx.kept(|s: &mut Compactor| s.setting);
    let view = shown.as_ref().map(super::shown_of);
    let how = super::rules::resume_of(view, cx.clock.now(), setting);
    let Some(shown) = shown else {
        return Ok(());
    };
    if !current_plan(cx, op) || how == Resume::None {
        return Ok(());
    }
    let armed: Option<Armed> = serde_json::from_value(shown["armed"].clone()).ok();
    let (Some(at), false) = (armed, how == Resume::Drop) else {
        disarm(cx, Some(op)).await?;
        let payback: Option<Payback> = serde_json::from_value(shown["payback"].clone()).ok();
        let ctx = shown["ctx"].as_f64().unwrap_or(0.);
        let text = plan_text(
            ctx,
            payback.as_ref(),
            Some("the next turn end, after a reload"),
            false,
        );
        show_plan(cx, out, "planned", Some(text), Map::new(), op).await?;
        let record = fields(vec![
            one("reason", json!("the plugin reloaded")),
            one("ctx_before", num(ctx)),
            payback_fields(payback.as_ref()),
        ]);
        log_compact(cx, out, "skip", record).await;
        return Ok(());
    };
    let at = Arc::new(at);
    cx.kept(|s: &mut Compactor| s.armed = Some(at.clone()));
    match how {
        Resume::Countdown { left_ms } => arm_timers(cx, out, &at, left_ms),
        _ if shown["kind"] == "countdown" => {
            // suggest now: a press compacts
            let why = shown["why"].as_str().unwrap_or("").to_string();
            offer_instead(cx, out, &at, &why, op).await?;
        }
        _ => (),
    }
    invalidate(cx).await;
    Ok(())
}

/// The person changed "Compact at the right moment" in /config: the countdown or offer standing
/// now follows it at once. Off cancels it; suggest makes a countdown an offer.
pub async fn setting_changed(cx: &Ctx, out: &Out, setting: Setting) -> Result<()> {
    cx.kept(|s: &mut Compactor| {
        s.setting = setting;
        s.loaded = true;
        s.setting_stamp += 1;
    });
    let op = plan_op(cx, None);
    let shown = view_of(cx);
    if !current_plan(cx, op) {
        return Ok(());
    }
    if setting == Setting::Off {
        let was = cx.kept(|s: &mut Compactor| s.armed.clone());
        disarm(cx, Some(op)).await?;
        show_plan(cx, out, "none", None, Map::new(), op).await?;
        if let Some(was) = was {
            let record = fields(vec![
                one("by", json!("setting")),
                one("mode", json!(was.mode.as_str())),
                one("ctx_before", num(was.ctx)),
                payback_fields(was.payback.as_ref()),
                seam_fields(&was.seam),
            ]);
            log_compact(cx, out, "cancel", record).await;
        }
    } else {
        let (armed, compacting) = cx.kept(|s: &mut Compactor| (s.armed.clone(), s.compacting));
        let counting = shown
            .as_ref()
            .is_some_and(|view| view["kind"] == "countdown");
        if let (Some(at), false, Setting::Suggest, true) = (armed, compacting, setting, counting) {
            let why = shown
                .as_ref()
                .and_then(|view| view["why"].as_str())
                .unwrap_or("");
            offer_instead(cx, out, &at, why, op).await?;
        }
    }
    invalidate(cx).await;
    Ok(())
}

/// A countdown keeps the identity it started with across every wait.
async fn start_countdown(cx: &Ctx, out: &Out, at: Arc<Armed>, op: PlanOp) -> Result<()> {
    let ends_at = cx.clock.now() + COUNTDOWN_MS;
    if !current_plan(cx, op) {
        return setting_moved(cx, out, &at, "countdown", op).await;
    }
    cancel_timers(cx);
    cx.kept(|s: &mut Compactor| s.armed = Some(at.clone()));
    let why = act_why(&at.seam);
    update(cx, "compact", |current| {
        if current_plan(cx, op) {
            Some(super::view_json("countdown", &at, &why, Some(ends_at)))
        } else {
            current
        }
    })?;
    if !current_plan(cx, op) {
        return Ok(());
    }
    arm_timers(cx, out, &at, COUNTDOWN_MS);
    let text = countdown_text(COUNTDOWN_MS, at.ctx, &why);
    let extra = fields(vec![
        one("ends_at", num(ends_at / 1000.)),
        one("ctx", num(at.ctx)),
        one("why", json!(why)),
    ]);
    show_plan(cx, out, "countdown", Some(text), extra, op).await?;
    let same = cx.kept(|s: &mut Compactor| {
        s.armed
            .as_ref()
            .is_some_and(|armed| Arc::ptr_eq(armed, &at))
    });
    if !current_plan(cx, op) || !same {
        return Ok(());
    }
    let record = fields(vec![
        one("mode", json!(at.mode.as_str())),
        one("forced", json!(at.forced)),
        one("ctx_before", num(at.ctx)),
        payback_fields(at.payback.as_ref()),
        one("ends_at", num(ends_at / 1000.)),
        one("why", json!(why)),
        seam_fields(&at.seam),
    ]);
    log_compact(cx, out, "countdown", record).await;
    invalidate(cx).await;
    Ok(())
}

/// What the removed `pane.py --compact-plan` gave, as the decision reads it.
fn payback_of(plan: Option<&Value>) -> Pay {
    let Some(plan) = plan else {
        return Pay::Unanswered;
    };
    match plan["payback"].as_f64() {
        Some(requests) => Pay::Is(Payback {
            requests,
            per_turn: plan["per_turn"].as_f64().filter(|per| *per > 0.),
        }),
        None => Pay::Never,
    }
}

/// A main-loop turn end: plan, offer, count down, compact, or nothing.
pub async fn at_turn_end(cx: &Ctx, out: &Out, answer: &str) -> Result<()> {
    let op = plan_op(cx, None);
    let usage = cx.host.call("session.usage", vec![]).await.ok();
    let ctx = usage
        .and_then(|usage| usage["context"]["tokens"].as_f64())
        .filter(|tokens| *tokens != 0.);
    let Some(ctx) = ctx.filter(|_| current_plan(cx, op)) else {
        return Ok(());
    };
    if ctx < BAND_LOW {
        // Under the band (a compaction came, the owner's or CSR's): nothing to plan, and a cancel
        // is forgotten.
        cx.kept(|s: &mut Compactor| s.cancelled_at = None);
        if has_plan(cx) {
            show_plan(cx, out, "none", None, Map::new(), op).await?;
        }
        return Ok(());
    }
    let plan = out.plan(&cx.sid, ctx as usize).await;
    if !current_plan(cx, op) {
        return Ok(());
    }
    let story = plan
        .as_ref()
        .and_then(|plan| plan["story"]["text"].as_str())
        .filter(|text| !text.is_empty());
    if let Some(story) = story {
        update(cx, "infinite", |current| {
            let plan = current.map_or(Value::Null, |row| row["plan"].clone());
            Some(json!({ "plan": plan, "story": story }))
        })?;
    }
    let pay = payback_of(plan.as_ref());
    let (running, asking, commit, gate, edits) =
        cx.kept(|s: &mut Compactor| (s.running(), s.asking, s.commit.clone(), s.gate, s.edits));
    let seam = seam_of(&TurnEnd {
        answer,
        asking: asking > 0,
        running: Some(&running),
        commit: commit.as_deref(),
        gate,
        edits,
    });
    let now = cx.clock.now();
    // the first this module saw of the session: the 10 minutes count from here
    let (last, loaded_at, setting, cancelled_at) = cx.kept(|s: &mut Compactor| {
        (
            s.last,
            *s.loaded_at.get_or_insert(now),
            s.setting,
            s.cancelled_at,
        )
    });
    let mode = mode_of(last, cx.clock.now(), loaded_at);
    if !current_plan(cx, op) {
        return Ok(());
    }
    let stamp = op.stamp;
    let native_window = out.native_window().await;
    let verdict = decide(&Ask {
        setting,
        mode,
        ctx,
        payback: pay,
        seam: &seam,
        cancelled_at,
        native_window,
    });
    let floor = plan.as_ref().and_then(|plan| plan["floor"].as_f64());
    let at = Arc::new(Armed {
        ctx,
        payback: pay.payback(),
        seam: seam.clone(),
        mode,
        forced: verdict.forced,
        floor,
        answer: answer.to_string(),
    });
    if verdict.act == Act::None {
        if has_plan(cx) {
            show_plan(cx, out, "none", None, Map::new(), op).await?;
        }
        return Ok(());
    }
    if matches!(verdict.act, Act::Plan | Act::Offer) {
        let wait = if verdict.act == Act::Plan {
            verdict.wait.as_deref()
        } else {
            Some("your press")
        };
        let text = plan_text(ctx, at.payback.as_ref(), wait, verdict.forced);
        let extra = fields(vec![
            one("ctx", num(ctx)),
            payback_fields(at.payback.as_ref()),
        ]);
        show_plan(cx, out, "planned", Some(text), extra, op).await?;
    }
    let record = fields(vec![
        one("act", json!(verdict.act.as_str())),
        one("mode", json!(mode.as_str())),
        one("forced", json!(verdict.forced)),
        one("ctx_before", num(ctx)),
        payback_fields(at.payback.as_ref()),
        one("floor", floor.map_or(Value::Null, num)),
        one("wait", json!(verdict.wait)),
        seam_fields(&seam),
    ]);
    log_compact(cx, out, "plan", record).await;
    match verdict.act {
        Act::Offer => {
            if !current_plan(cx, op) {
                return setting_moved(cx, out, &at, "offer", op).await;
            }
            cx.kept(|s: &mut Compactor| s.armed = Some(at.clone()));
            let why = act_why(&seam);
            update(cx, "compact", |current| {
                if current_plan(cx, op) {
                    Some(super::view_json("offer", &at, &why, None))
                } else {
                    current
                }
            })?;
            invalidate(cx).await;
        }
        Act::Countdown => start_countdown(cx, out, at, op).await?,
        Act::Compact => compact_now(cx, out, at, "unattended", Some(stamp), Some(op)).await?,
        _ => (),
    }
    Ok(())
}

/// A turn that starts during a countdown or an offer (a notification came in) ends it: the next
/// turn end decides.
pub async fn turn_started(cx: &Ctx, out: &Out) -> Result<()> {
    let op = plan_op(cx, None);
    let now = cx.clock.now();
    cx.kept(|s: &mut Compactor| {
        s.turn_running = true;
        s.turn_at = now;
    });
    if !current_plan(cx, op) {
        return Ok(());
    }
    let (was, compacting) = cx.kept(|s: &mut Compactor| {
        s.loaded_at.get_or_insert(now);
        (s.armed.clone(), s.compacting)
    });
    if compacting {
        return show_plan(cx, out, "none", None, Map::new(), op).await;
    }
    let Some(was) = was else {
        return Ok(());
    };
    disarm(cx, Some(op)).await?;
    let record = fields(vec![
        one("reason", json!("a turn started")),
        one("mode", json!(was.mode.as_str())),
        one("ctx_before", num(was.ctx)),
        seam_fields(&was.seam),
    ]);
    log_compact(cx, out, "skip", record).await;
    let text = plan_text(
        was.ctx,
        was.payback.as_ref(),
        Some("a turn is running"),
        was.forced,
    );
    show_plan(cx, out, "planned", Some(text), Map::new(), op).await
}

/// The Stop hook's own list of background work, as the keep list reads it.
pub fn stopped(at: f64, tasks: &[Value]) -> Stopped {
    let text = |task: &Value, name: &str| task[name].as_str().unwrap_or("").to_string();
    Stopped {
        at,
        running: (tasks.iter())
            .map(|task| super::rules::Running {
                id: text(task, "id"),
                kind: text(task, "type"),
                description: text(task, "description"),
            })
            .collect(),
    }
}
