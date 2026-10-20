//! The band above the prompt: the held digit, the note lifecycle at a prompt, and the band
//! itself, ported from the removed TypeScript companion's `AbovePrompt` band.
//!
//! The entries:
//! - `digit`, first: a prompt that is only a digit, sent just after a band note drew,
//!   is a hotkey that missed. It is held with a toast (`{ drop }`), and sent on if the same digit
//!   comes again within 10 s.
//! - `digit`, late: every prompt a note survives counts; the second one clears it,
//!   and a dream cleared that way was typed past (`dreamer::typed_past`).
//! - `band`, the render: the compactor's countdown or offer first, else the walkthrough's next
//!   step, else the note (offer, explained, asked, the coaching `term`).
//! - `press`: the note's buttons, and the compactor's two (`compact-now`, `compact-cancel`).
//!   The walkthrough's buttons are `setup_walk`'s own.
//!
//! What the TypeScript kept in module variables (`held`, `bandAt`, `waiting`) is `BandState`,
//! kept for the life of the process. A button is a node with a `key` of its own; the relay posts
//! the key back, and the press looks the key up among the buttons the session's `view` row draws
//! now, as the TypeScript's closures did with the `shown` they had been drawn from.

mod nodes;
#[cfg(test)]
pub(crate) mod tests;

use super::{
    compactor::{self, outside::Outside as CompactOutside},
    dreamer::{
        answered,
        text::{ask_claude, clean_for_draft, seen_entry},
        typed_past,
    },
    fleet::text::js_trim,
    guard, pane_cmd, setup_walk,
    state::{config_row, config_rows, is_person, remember, update, Options, KEPT},
};
use crate::companion::relay::dispatch::{handler, Ctx, Handler, Reply, Table, PRESS};
use anyhow::Result;
use nodes::{asks, note_node, over, walk_node};
use serde_json::{json, Value};
use std::sync::Arc;

/// A bare digit this soon after a band note showed is a hotkey miss (the TypeScript's
/// `BAND_WINDOW_MS`).
const BAND_WINDOW_MS: f64 = 30_000.;
/// The same digit sent again this soon goes through (the TypeScript's `AGAIN_MS`).
const AGAIN_MS: f64 = 10_000.;
/// A note nobody answers goes after this many prompts.
const CLEAR_AFTER_PROMPTS: f64 = 2.;
/// The last dreams "already thought of", kept.
const SEEN_KEPT: usize = 50;

/// What the band keeps between calls: the TypeScript's `held`, `bandAt` and `waiting`.
#[derive(Default)]
struct BandState {
    /// A bare digit kept back, and when.
    held: Option<(String, f64)>,
    /// When another band (You should know's) last drew a question.
    band_at: f64,
    /// True while another band that asks, or the compactor's, has the place: the note waits, and
    /// a prompt typed meanwhile is not typed past it.
    waiting: bool,
}

/// What the band touches outside this process, so a test brings its own.
#[derive(Clone)]
pub struct Seams {
    /// The compactor's outside: `compact-now` and `compact-cancel` act through it.
    pub compact: Arc<dyn CompactOutside>,
    /// The person's CLAUDE.md, for the coaching note's 1.
    pub machine: Arc<dyn guard::Machine>,
    /// The pane, for "Open the CSR pane".
    pub pane: Arc<dyn pane_cmd::Outside>,
}

impl Seams {
    pub fn real() -> Self {
        Self {
            compact: Arc::new(compactor::outside::Here::new()),
            machine: guard::real(),
            pane: pane_cmd::real(),
        }
    }
}

/// The TypeScript's `isBareDigit`: `/^\s*\d\s*$/`.
fn is_bare_digit(text: &str) -> bool {
    let mut chars = js_trim(text).chars();
    matches!((chars.next(), chars.next()), (Some(c), None) if c.is_ascii_digit())
}

/// The session's `view` row: the note up, or none.
fn view_row(cx: &Ctx) -> Option<Value> {
    cx.store
        .get(&cx.sid, "view")
        .ok()
        .flatten()
        .filter(|view| !view.is_null())
}

async fn invalidate(cx: &Ctx) {
    let _ = cx.host.call("ui.invalidate", vec![]).await;
}

/// JavaScript's truthiness, for the values a tree and a reply hold.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(false) => false,
        Value::String(text) => !text.is_empty(),
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.),
        _ => true,
    }
}

// ---------------------------------------------------------------- digit

/// The held digit.
fn on_hold() -> Handler {
    handler(|cx, e| async move {
        let at = cx.clock.now();
        let yours = matches!(e["origin"]["kind"].as_str(), Some("composer" | "bridge"));
        let text = e["text"].as_str().unwrap_or("");
        if !yours || !is_bare_digit(text) {
            return Ok(Reply::none());
        }
        let digit = js_trim(text).to_string();
        let shown_lately = view_row(&cx).is_some_and(|up| {
            let shown_at = up["shownAt"].as_f64().unwrap_or(f64::NEG_INFINITY);
            up["kind"] != "asked" && at - shown_at <= BAND_WINDOW_MS
        });
        let (held, band_at) = cx.kept(|b: &mut BandState| (b.held.clone(), b.band_at));
        if held.is_some_and(|(text, was)| text == digit && at - was <= AGAIN_MS) {
            cx.kept(|b: &mut BandState| b.held = None);
        } else if shown_lately || (band_at > 0. && at - band_at <= BAND_WINDOW_MS) {
            cx.kept(|b: &mut BandState| b.held = Some((digit.clone(), at)));
            let said = format!(
                "“{digit}” looks like a band hotkey. Press it on the band, or send it again \
                 within 10 s to give it to Claude."
            );
            let _ = cx.host.call("ui.toast", vec![json!(said)]).await;
            return Ok(Reply::answer(json!({ "drop": said })));
        }
        Ok(Reply::none())
    })
}

/// The note lifecycle: a prompt a note survives is counted, the second
/// clears it, and an offer cleared that way was typed past.
fn on_lifecycle() -> Handler {
    handler(|cx, _e| async move {
        if view_row(&cx).is_none() {
            return Ok(Reply::none());
        }
        let waiting = cx.kept(|b: &mut BandState| b.waiting);
        let (mut ignored, mut changed) = (None, false);
        update(&cx, "view", |current| {
            let mut view = current?;
            if waiting {
                return Some(view); // a note nobody saw was not typed past
            }
            changed = true;
            let survived = view["promptsSurvived"].as_f64().map(|n| n + 1.);
            if let Some(survived) = survived.filter(|n| *n < CLEAR_AFTER_PROMPTS) {
                view["promptsSurvived"] = json!(survived as i64);
                if let Some(fields) = view.as_object_mut() {
                    fields.remove("message");
                }
                return Some(view);
            }
            if view["kind"] == "offer" {
                ignored = Some(view["note"].clone());
            }
            None
        })?;
        if changed {
            invalidate(&cx).await;
        }
        if let Some(note) = ignored {
            // Typed past: ask less often, as You should know does.
            typed_past(&cx, &note).await?;
        }
        Ok(Reply::none())
    })
}

// ---------------------------------------------------------------- the render

fn set_waiting(cx: &Ctx, waiting: bool) {
    cx.kept(|b: &mut BandState| b.waiting = waiting);
}

/// `ui.render` of the TypeScript's `AbovePrompt`. `e.beneath` is what the bands below
/// this one drew, as plain data; no tree back is `beneath` as it is.
fn on_band() -> Handler {
    handler(|cx, e| async move {
        let beneath = e["beneath"].clone();
        let asking = asks(&beneath);
        let now = cx.clock.now();
        if asking {
            cx.kept(|b: &mut BandState| b.band_at = now);
        }
        if truthy(&e["props"]["hasSurvey"]) {
            return Ok(Reply::none());
        }
        // A compaction counting down, or offered, comes first. Under another band that asks, it
        // draws no hotkeys of its own (two on one hotkey: the later wins).
        if let Some(counting) = compactor::compact_band_quiet(&cx, asking).await {
            set_waiting(&cx, view_row(&cx).is_some());
            return Ok(Reply::tree(over(counting, &beneath)));
        }
        // The walkthrough next, one step at a time; under another band that asks, it waits.
        let setup = setup_walk::view_of(&cx);
        let step = if asking {
            None
        } else {
            setup_walk::next_step(setup.as_ref())
        };
        if step.is_some() && is_person(&cx).await {
            // A bare digit sent as a prompt is a hotkey that missed: held, as for a note.
            cx.kept(|b: &mut BandState| b.band_at = now);
            set_waiting(&cx, view_row(&cx).is_some());
            let options = Options::of(&config_rows(&cx).await);
            if let Some(walk) = setup_walk::walk(setup.as_ref(), &options) {
                return Ok(Reply::tree(over(walk_node(&walk), &beneath)));
            }
        }
        // One question at a time. Two bands both on 1, 2, 4 and 0 would answer the lower one, so
        // 4 under a dream would turn You should know off. The note waits until that band clears.
        let shown = view_row(&cx);
        let waiting = shown.is_some() && asking;
        set_waiting(&cx, waiting);
        let Some(shown) = shown.filter(|_| !waiting) else {
            return Ok(Reply::none());
        };
        Ok(Reply::tree(over(note_node(&shown), &beneath)))
    })
}

// ---------------------------------------------------------------- the buttons

/// One button of a note's band: what `press` finds the key among.
struct Button {
    key: &'static str,
    hotkey: &'static str,
    label: String,
    dim: bool,
    dismiss: bool,
}

fn button(key: &'static str, hotkey: &'static str, label: &str) -> Button {
    Button {
        key,
        hotkey,
        label: label.to_string(),
        dim: false,
        dismiss: false,
    }
}

fn dismissing(key: &'static str, label: &str) -> Button {
    Button {
        dismiss: true,
        ..button(key, "0", label)
    }
}

/// You should know's way out: a dim 4 that asks for a second press, then turns dreams off.
fn way_out(view: &Value) -> Vec<Button> {
    if view["note"]["kind"] != "dream" {
        return Vec::new();
    }
    let label = if view["confirmOff"] == true {
        "Press again to turn dreams off"
    } else {
        "Disable"
    };
    vec![Button {
        dim: true,
        ..button("band-disable", "4", label)
    }]
}

/// The buttons the note in `view` draws, left to right.
fn buttons_of(view: &Value) -> Vec<Button> {
    let dream = view["note"]["kind"] == "dream";
    if view["note"]["kind"] == "term" {
        return vec![
            button("band-add", "1", "Add to CLAUDE.md"),
            dismissing("band-term-dismiss", "Dismiss"),
        ];
    }
    let mut row = Vec::new();
    match view["kind"].as_str() {
        Some("offer") => {
            row.push(button("band-learn", "1", "Learn more"));
            let knew = if dream {
                "Already thought of it"
            } else {
                "Knew this already"
            };
            row.push(button("band-know", "2", knew));
            row.extend(way_out(view));
            row.push(dismissing("band-dismiss", "Dismiss"));
        }
        Some("explained") => {
            row.push(button("band-understood", "1", "Understood"));
            let chat = if dream {
                "Ask Claude"
            } else {
                "Chat in main session"
            };
            row.push(button("band-chat", "2", chat));
            row.push(button("band-pane", "3", "Open the CSR pane"));
            row.push(dismissing("band-dismiss", "Dismiss"));
        }
        _ => {
            row.push(button("band-helpful", "1", "That was helpful"));
            row.push(button("band-not-relevant", "2", "Not relevant"));
            row.extend(way_out(view));
            row.push(dismissing("band-close", "Close"));
        }
    }
    row
}

/// `{ ...shown, ...changes }`, where a null in `changes` takes the field out (`undefined`).
fn with(shown: &Value, changes: Value) -> Value {
    let mut view = shown.clone();
    if let (Some(fields), Some(changes)) = (view.as_object_mut(), changes.as_object()) {
        for (name, value) in changes {
            if value.is_null() {
                fields.remove(name);
            } else {
                fields.insert(name.clone(), value.clone());
            }
        }
    }
    view
}

fn set_view(cx: &Ctx, view: Option<Value>) -> Result<()> {
    update(cx, "view", |_| view)?;
    Ok(())
}

/// Answer the note and leave it as `then` (none: gone).
async fn close(cx: &Ctx, note: &Value, answer: &str, then: Option<Value>) -> Result<()> {
    answered(cx, note, answer, false, None).await?;
    set_view(cx, then)
}

/// The note asks whether it was worth it, once.
fn asked_view(shown: &Value) -> Value {
    with(shown, json!({ "kind": "asked", "message": null }))
}

/// A note handed to Claude as text in the prompt box (the TypeScript's `handoff`).
fn handoff(shown: &Value) -> String {
    let note = &shown["note"];
    let word = |name: &str| note[name].as_str().unwrap_or("").to_string();
    if note["kind"] == "dream" {
        return ask_claude(&word("line"), &word("title"), &word("text"));
    }
    let head = |name: &str, n: usize| word(name).chars().take(n).collect::<String>();
    let mut receipts = Vec::new();
    if !word("oid").is_empty() {
        receipts.push(format!("commit {}", head("oid", 7)));
    }
    receipts.push(format!("session {}", head("conversation", 8)));
    let tag = shown["tag"].as_str().unwrap_or("");
    let rows = [
        "Here is a note offered by CSR (claude-self-reflect):".to_string(),
        String::new(),
        format!("> {tag} · {}", word("line")),
        ">".to_string(),
        format!("> {}", word("text")),
        ">".to_string(),
        format!("> Receipts: {}.", receipts.join(", ")),
    ];
    clean_for_draft(&rows.join("\n"))
}

/// `$.config.set` came back refused, or threw.
fn refused(done: &Result<Value>) -> bool {
    match done {
        Err(_) => true,
        Ok(done) => truthy(&done["deny"]),
    }
}

/// A second press of the dim 4 turns dreams off; the first asks for it.
async fn turn_off(cx: &Ctx, shown: &Value) -> Result<()> {
    if shown["confirmOff"] != true {
        return set_view(cx, Some(with(shown, json!({ "confirmOff": true }))));
    }
    answered(cx, &shown["note"], "turn_off", false, None).await?;
    let key = config_row(&config_rows(cx).await, "dreams");
    let done = cx
        .host
        .call("config.set", vec![json!({ "key": key, "value": false })])
        .await;
    let said = "Couldn't turn dreams off here. Use /config.";
    let view = refused(&done).then(|| with(shown, json!({ "confirmOff": false, "message": said })));
    set_view(cx, view)
}

/// Chat in main session / Ask Claude: the note into the prompt box, if the box is empty.
async fn chat(cx: &Ctx, shown: &Value) -> Result<()> {
    let boxed = cx.host.call("prompt.read", vec![]).await.ok();
    if boxed.is_some_and(|read| !js_trim(read["text"].as_str().unwrap_or("")).is_empty()) {
        let said = "Your prompt box has text in it. Send or clear it, then press 2 again.";
        return set_view(cx, Some(with(shown, json!({ "message": said }))));
    }
    let fill = json!({ "text": handoff(shown), "mode": "replace" });
    let filled = cx.host.call("prompt.fill", vec![fill]).await.ok();
    if filled.is_some_and(|done| done["isFilled"] == true) {
        let dream = shown["note"]["kind"] == "dream";
        let answer = if dream { "ask_claude" } else { "chat" };
        return close(cx, &shown["note"], answer, None).await;
    }
    let said = "The prompt box didn't take it. Press 2 to try again.";
    set_view(cx, Some(with(shown, json!({ "message": said }))))
}

/// Knew this already / Already thought of it: never offered again.
async fn know(cx: &Ctx, shown: &Value) -> Result<()> {
    let note = &shown["note"];
    if note["kind"] == "dream" {
        let entry = seen_entry(note["line"].as_str().unwrap_or(""), note["title"].as_str());
        remember(cx, "knownDreams", &entry, SEEN_KEPT)?;
        return close(cx, note, "thought_of_it", None).await;
    }
    remember(cx, "known", note["id"].as_str().unwrap_or(""), KEPT)?;
    close(cx, note, "know_it", None).await
}

/// The coaching note's 1: one line under "## Terms" of the person's CLAUDE.md.
async fn add_term(cx: &Ctx, seams: &Seams, shown: &Value) -> Result<()> {
    let note = &shown["note"];
    if guard::add_to_claude_md(&seams.machine, note["text"].as_str().unwrap_or("")).await {
        let said = "Added to your CLAUDE.md under Terms. Edit the line to say what you mean.";
        let _ = cx.host.call("ui.toast", vec![json!(said)]).await;
        return close(cx, note, "added", None).await;
    }
    let said = "Couldn't write your CLAUDE.md. Nothing was changed.";
    set_view(cx, Some(with(shown, json!({ "message": said }))))
}

/// What a press of `key` does to the note in `shown`.
async fn act(cx: &Ctx, seams: &Seams, key: &str, shown: &Value) -> Result<()> {
    let note = &shown["note"];
    match key {
        "band-learn" => set_view(cx, Some(with(shown, json!({ "kind": "explained" })))),
        "band-know" => know(cx, shown).await,
        "band-disable" => turn_off(cx, shown).await,
        "band-dismiss" => close(cx, note, "dismiss", Some(asked_view(shown))).await,
        "band-understood" => close(cx, note, "got_it", Some(asked_view(shown))).await,
        "band-chat" => chat(cx, shown).await,
        "band-pane" => {
            pane_cmd::open_pane(cx, seams.pane.as_ref(), &cx.cwd).await;
            Ok(())
        }
        "band-helpful" => close(cx, note, "helpful", None).await,
        "band-not-relevant" => close(cx, note, "not_relevant", None).await,
        "band-close" => set_view(cx, None),
        "band-add" => add_term(cx, seams, shown).await,
        "band-term-dismiss" => close(cx, note, "dismiss", None).await,
        _ => Ok(()),
    }
}

/// The band's own buttons: the compactor's two, and the note's by the key they were drawn with.
fn on_press(seams: Seams) -> Handler {
    handler(move |cx, e| {
        let seams = seams.clone();
        async move {
            let key = e["key"].as_str().unwrap_or("");
            if key.starts_with("compact-") {
                compactor::on_press_with(&cx, &seams.compact, key).await?;
                return Ok(Reply::none());
            }
            // A press that is late for its note (it is gone, or has moved on) is not one.
            let Some(shown) = view_row(&cx).filter(|_| key.starts_with("band-")) else {
                return Ok(Reply::none());
            };
            if buttons_of(&shown).iter().any(|button| button.key == key) {
                // A failed press leaves the note as it was (the button's own catch).
                let _ = act(&cx, &seams, key, &shown).await;
                invalidate(&cx).await;
            }
            Ok(Reply::none())
        }
    })
}

// ---------------------------------------------------------------- the entries

pub fn register_with(t: &mut Table, seams: Seams) {
    t.on("digit", "band", on_hold());
    t.on("band", "band", on_band());
    t.on(PRESS, "band", on_press(seams));
}

pub fn register(t: &mut Table) {
    register_with(t, Seams::real());
}

/// The entries that run after other features' on the same key (`app::table`).
pub fn register_late(t: &mut Table) {
    t.on("digit", "band", on_lifecycle());
}
