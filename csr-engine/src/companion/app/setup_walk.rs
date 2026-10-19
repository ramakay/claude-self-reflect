//! The first run's walkthrough: each setting a person should choose, one step at a time above the
//! prompt. Port of `hooks/setup.ts` and of register.tsx's `STEPS`/`startSetup`/`setAside`/
//! `answerStep` (lines 941-983) with the press and command wiring they have there.
//!
//! The band draws the walkthrough (a later package's); `walk` hands it what to draw as data. The
//! view is the session's `setup` row, `{ steps, done, aside, message? }`; each answer is the
//! machine's `setup:answer:<step>` row and each start that showed a step the machine's
//! `setup:seen:<step>` list. Every change asks Claude Code to draw the band again.

use super::state::{self, config_row, config_rows, strings, update, Options, BAND_HIGH, BAND_LOW};
use crate::companion::{
    relay::dispatch::{handler, Ctx, Handler, Reply, Table, PRESS},
    store::MACHINE,
};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;

/// The settings, in the order they are asked (a step added later shows alone).
pub const STEPS: [&str; 2] = ["dreams", "autoCompact"];
/// A step set aside without an answer shows at this many session starts, then never; `/csr setup`
/// reopens it.
pub const SETUP_STARTS: usize = 2;

/// The `/config` rows' titles, as plugin.json's userConfig names them: where a refused write sends
/// the person.
pub fn title(step: &str) -> &'static str {
    match step {
        "dreams" => "Dreams",
        _ => "Compact at the right moment",
    }
}

/// One key of a step: what it writes to `/config` and the label it carries.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub hotkey: &'static str,
    pub label: &'static str,
    pub value: Value,
}

pub fn choices(step: &str) -> Vec<Choice> {
    let choice = |hotkey, label, value| Choice {
        hotkey,
        label,
        value,
    };
    match step {
        "dreams" => vec![
            choice("1", "On", json!(true)),
            choice("0", "Off", json!(false)),
        ],
        _ => vec![
            choice("1", "Auto", json!("auto")),
            choice("2", "Suggest", json!("suggest")),
            choice("0", "Off", json!("off")),
        ],
    }
}

/// What the step says above its keys; each line is meant to fit one row of a 100-column terminal.
pub fn lines(step: &str) -> Vec<String> {
    let text: Vec<String> = match step {
        "dreams" => vec![
            "Dreams: while Claude works, a side agent asks one \"what if…?\" when CSR's memory \
             gives a reason."
                .into(),
            "It looks only when CSR has something new, and checks before each look that its \
             spend stays within"
                .into(),
            "1 token for every 10 Claude spends. A look can run over; the next waits until \
             Claude has caught up."
                .into(),
        ],
        _ => vec![
            format!(
                "Compact for you: CSR compacts at a quiet turn end once context reaches {}–{}.",
                state::k(BAND_LOW),
                state::k(BAND_HIGH)
            ),
            "Auto: on its own. A 20 s countdown at the terminal; at once from the phone or after \
             10 min away."
                .into(),
            "Suggest (the default): it offers, a press compacts. Off: never.".into(),
        ],
    };
    text
}

pub fn heading(step: &str) -> String {
    let at = STEPS.iter().position(|s| *s == step).map_or(0, |i| i + 1);
    format!("CSR setup {at}/{}", STEPS.len())
}

pub fn refused_text(plugin: &str, step: &str) -> String {
    format!(
        "Couldn't set it here. Use /config: {plugin}, {}.",
        title(step)
    )
}

/// The steps a session start shows: each not answered yet, unless it was already set aside at
/// `SETUP_STARTS` earlier starts. A start that already showed it (a reload) shows it again.
pub fn due_steps(
    answered: &HashSet<&str>,
    seen: &dyn Fn(&str) -> Vec<String>,
    session: &str,
) -> Vec<&'static str> {
    STEPS
        .iter()
        .copied()
        .filter(|step| {
            let starts = seen(step);
            !answered.contains(step)
                && (starts.iter().any(|s| s == session) || starts.len() < SETUP_STARTS)
        })
        .collect()
}

/// The session's `setup` row.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SetupView {
    pub steps: Vec<String>,
    pub done: Vec<String>,
    pub aside: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// The step the band shows next: none when there is no walkthrough, it is set aside, or all of its
/// steps are answered.
pub fn next_step(shown: Option<&SetupView>) -> Option<String> {
    let shown = shown.filter(|view| !view.aside)?;
    shown
        .steps
        .iter()
        .find(|s| !shown.done.contains(s))
        .cloned()
}

pub fn view_of(cx: &Ctx) -> Option<SetupView> {
    let row = cx.store.get(&cx.sid, "setup").ok()??;
    serde_json::from_value(row).ok()
}

fn change_view(
    cx: &Ctx,
    change: impl FnOnce(Option<SetupView>) -> Option<SetupView>,
) -> Result<()> {
    update(cx, "setup", |row| {
        let view = row.and_then(|row| serde_json::from_value(row).ok());
        change(view).and_then(|view| serde_json::to_value(view).ok())
    })?;
    Ok(())
}

/// What the band draws for the step in force.
#[derive(Clone, Debug, PartialEq)]
pub struct Walk {
    pub step: String,
    /// `CSR setup 1/2`, drawn dim before the first line.
    pub heading: String,
    /// The first line follows the heading; the rest sit under it.
    pub lines: Vec<String>,
    /// Why the last answer was not written, when it was refused.
    pub message: Option<String>,
    pub choices: Vec<WalkChoice>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WalkChoice {
    /// The button's press key.
    pub key: String,
    pub hotkey: &'static str,
    /// The label, `(now)` after it on the one in force.
    pub label: String,
    pub value: Value,
}

/// The value of `step` in force, as `Choice::value` writes it.
pub fn current(step: &str, options: &Options) -> Value {
    match step {
        "dreams" => json!(options.dreams),
        _ => json!(options.auto_compact),
    }
}

/// What the band draws now: none when no step is due. The band also holds its own conditions
/// (a person at the prompt, no other band asking) before it draws this.
pub fn walk(shown: Option<&SetupView>, options: &Options) -> Option<Walk> {
    let step = next_step(shown)?;
    let now = current(&step, options);
    Some(Walk {
        heading: heading(&step),
        lines: lines(&step),
        message: shown.and_then(|view| view.message.clone()),
        choices: choices(&step)
            .into_iter()
            .map(|choice| WalkChoice {
                key: format!("setup-{step}-{}", choice.hotkey),
                hotkey: choice.hotkey,
                label: if choice.value == now {
                    format!("{} (now)", choice.label)
                } else {
                    choice.label.to_string()
                },
                value: choice.value,
            })
            .collect(),
        step,
    })
}

/// At a session start: the steps due here, each counted as shown at this start. A reload (a
/// setting just written reloads the module) starts the session again: the walkthrough keeps its
/// place.
pub fn start_setup(cx: &Ctx) -> Result<()> {
    if view_of(cx).is_some() {
        return Ok(());
    }
    let answered: HashSet<&str> = STEPS
        .iter()
        .copied()
        .filter(|step| {
            let row = cx.store.get(MACHINE, &format!("setup:answer:{step}"));
            matches!(row, Ok(Some(_)))
        })
        .collect();
    let seen = |step: &str| strings(cx, MACHINE, &format!("setup:seen:{step}"));
    let steps = due_steps(&answered, &seen, &cx.sid);
    for step in &steps {
        let mut starts = seen(step);
        if !starts.contains(&cx.sid) {
            starts.push(cx.sid.clone());
            cx.store
                .set(MACHINE, &format!("setup:seen:{step}"), &json!(starts))?;
        }
    }
    change_view(cx, |_| {
        Some(SetupView {
            steps: steps.iter().map(|s| s.to_string()).collect(),
            ..SetupView::default()
        })
    })
}

async fn invalidate(cx: &Ctx) {
    let _ = cx.host.call("ui.invalidate", vec![]).await;
}

/// Typed past: set aside for this session.
pub async fn set_aside(cx: &Ctx) -> Result<()> {
    if next_step(view_of(cx).as_ref()).is_none() {
        return Ok(());
    }
    change_view(cx, |shown| {
        shown.map(|view| SetupView {
            aside: true,
            message: None,
            ..view
        })
    })?;
    invalidate(cx).await;
    Ok(())
}

/// An answer is kept first, then written to /config. `current` is the step's value as it stood.
pub async fn answer_step(cx: &Ctx, step: &str, value: &Value, current: &Value) -> Result<()> {
    let wrote = write_answer(cx, step, value, current).await;
    invalidate(cx).await;
    wrote
}

async fn write_answer(cx: &Ctx, step: &str, value: &Value, current: &Value) -> Result<()> {
    cx.store.set(
        MACHINE,
        &format!("setup:answer:{step}"),
        &json!({ "value": value, "at": cx.clock.now() }),
    )?;
    change_view(cx, |shown| {
        shown.map(|mut view| {
            view.done.push(step.to_string());
            view.message = None;
            view
        })
    })?;
    if value == current {
        return Ok(());
    }
    let key = config_row(&config_rows(cx).await, step);
    let refused = match cx
        .host
        .call("config.set", vec![json!({ "key": key, "value": value })])
        .await
    {
        Ok(done) => state_denies(&done),
        Err(_) => true,
    };
    if !refused {
        return Ok(());
    }
    let message = refused_text("csr-companion", step);
    change_view(cx, |shown| {
        shown.map(|view| SetupView {
            message: Some(message.clone()),
            ..view
        })
    })?;
    if next_step(view_of(cx).as_ref()).is_none() {
        // The last step: no band left to say it on.
        let _ = cx.host.call("ui.toast", vec![json!(message)]).await;
    }
    Ok(())
}

/// `$.config.set` answered with a `deny` that says something.
fn state_denies(done: &Value) -> bool {
    done.get("deny").is_some_and(|deny| match deny {
        Value::Null | Value::Bool(false) => false,
        Value::String(text) => !text.is_empty(),
        _ => true,
    })
}

/// `/csr setup`: every step comes back. The answer the command gives.
pub async fn command_setup(cx: &Ctx) -> Result<String> {
    change_view(cx, |_| {
        Some(SetupView {
            steps: STEPS.iter().map(|s| s.to_string()).collect(),
            ..SetupView::default()
        })
    })?;
    invalidate(cx).await;
    Ok(format!(
        "CSR setup is above the prompt: {} steps, a key press each.",
        STEPS.len()
    ))
}

/// `setup-<step>-<hotkey>`: the step and its choice, when the key names one.
fn choice_of(key: &str) -> Option<(&'static str, Choice)> {
    let rest = key.strip_prefix("setup-")?;
    STEPS.iter().copied().find_map(|step| {
        let hotkey = rest.strip_prefix(step)?.strip_prefix('-')?;
        let choice = choices(step).into_iter().find(|c| c.hotkey == hotkey)?;
        Some((step, choice))
    })
}

fn on_start() -> Handler {
    handler(|cx, e| async move {
        if e["isInteractive"].as_bool() != Some(true) {
            return Ok(Reply::none());
        }
        let _ = start_setup(&cx); // the walkthrough never fails a start
        Ok(Reply::none())
    })
}

/// Typing sets it aside; a lone digit into an empty box is a band hotkey on its way, not typing.
fn on_edit() -> Handler {
    handler(|cx, e| async move {
        let key = e["key"]["key"].as_str().unwrap_or("");
        let lone_digit = e["text"].as_str() == Some("")
            && key.len() == 1
            && key.bytes().all(|b| b.is_ascii_digit());
        if !lone_digit {
            let _ = set_aside(&cx).await;
        }
        Ok(Reply::none())
    })
}

/// A prompt you sent past the walkthrough.
fn on_digit() -> Handler {
    handler(|cx, e| async move {
        let kind = e["origin"]["kind"].as_str();
        if matches!(kind, Some("composer" | "bridge")) {
            let _ = set_aside(&cx).await;
        }
        Ok(Reply::none())
    })
}

fn on_press() -> Handler {
    handler(|cx, e| async move {
        let Some((step, choice)) = e["key"].as_str().and_then(choice_of) else {
            return Ok(Reply::none());
        };
        // Only the step the band shows can be answered; a press that is late for it is not one.
        if next_step(view_of(&cx).as_ref()).as_deref() != Some(step) {
            return Ok(Reply::none());
        }
        let options = Options::of(&config_rows(&cx).await);
        let _ = answer_step(&cx, step, &choice.value, &current(step, &options)).await;
        Ok(Reply::none())
    })
}

/// The walkthrough's entries, each where the TypeScript runs its group:
/// `start` (register.tsx 2393), `edit` (1965), `digit` (2141) and the buttons' presses.
pub fn register(t: &mut Table) {
    t.on("start", "setup_walk", on_start());
    t.on("edit", "setup_walk", on_edit());
    t.on("digit", "setup_walk", on_digit());
    t.on(PRESS, "setup_walk", on_press());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::companion::{
        app::pane_cmd,
        relay::{
            dispatch::testing::{fixture_in, run, Fixture},
            host::fake::World,
        },
    };

    /// What a person who never chose has in force: dreams on (no row says otherwise), compaction at
    /// suggest.
    fn fresh() -> Value {
        json!([{ "key": "csr-companion.autoCompact", "value": "suggest" },
            { "key": "csr-companion.dreams", "value": true }])
    }

    /// A fake Claude Code whose `/config` answers `rows` for the next 40 reads.
    fn world(rows: Value, refused: bool) -> World {
        let mut world = World {
            config_refused: refused,
            ..World::default()
        };
        let list = world.script.entry("config.list".into()).or_default();
        for _ in 0..40 {
            list.push_back(Ok(rows.clone()));
        }
        world
    }

    fn table() -> Table {
        let mut table = Table::new();
        pane_cmd::register(&mut table);
        register(&mut table);
        table
    }

    fn start() -> Value {
        json!({ "isInteractive": true, "surface": "terminal" })
    }

    struct Walking {
        fx: Fixture,
        table: Table,
    }

    /// A first run: no step answered, nothing seen, in session `sid`.
    fn first_run(sid: &str, refused: bool) -> Walking {
        Walking {
            fx: fixture_in(world(fresh(), refused), sid),
            table: table(),
        }
    }

    impl Walking {
        async fn start(&self, e: Value) {
            run(&self.table, "start", &self.fx.cx, e).await;
        }

        async fn press(&self, key: &str) {
            run(&self.table, PRESS, &self.fx.cx, json!({ "key": key })).await;
        }

        async fn edit(&self, key: &str) {
            let e = json!({ "origin": { "kind": "composer" }, "text": "", "cursor": 0,
                "start": 0, "end": 0, "inputText": key, "key": { "key": key } });
            run(&self.table, "edit", &self.fx.cx, e).await;
        }

        async fn walk(&self) -> Option<Walk> {
            let options = Options::of(&config_rows(&self.fx.cx).await);
            walk(view_of(&self.fx.cx).as_ref(), &options)
        }

        /// The row the band draws a step from: `CSR setup 1/2`, ...
        async fn heading(&self) -> Option<String> {
            self.walk().await.map(|walk| walk.heading)
        }

        fn configured(&self) -> Vec<Value> {
            self.fx.host.seen.lock().unwrap().configured.clone()
        }

        fn toasts(&self) -> Vec<String> {
            self.fx.host.seen.lock().unwrap().toasts.clone()
        }

        fn machine(&self, key: &str) -> Option<Value> {
            self.fx.cx.store.get(MACHINE, key).unwrap()
        }
    }

    fn button(walk: &Walk, label: &str) -> bool {
        walk.choices.iter().any(|choice| choice.label == label)
    }

    #[tokio::test]
    async fn the_plugin_just_added_two_steps_above_the_prompt_each_digit_writes_its_config() {
        let w = first_run("s", false);
        w.start(start()).await;
        let walk = w.walk().await.unwrap();
        assert_eq!(walk.heading, "CSR setup 1/2");
        let said = walk.lines.join(" ");
        // "not at most": a fork has no token cap.
        assert!(said.contains("checks before each look that its spend stays within"));
        assert!(said.contains("A look can run over; the next waits until Claude has caught up"));
        assert!(button(&walk, "On (now)"), "dreams start on");
        assert!(button(&walk, "Off"));
        w.press("setup-dreams-0").await;
        assert_eq!(
            w.configured(),
            [json!({ "key": "csr-companion.dreams", "value": false })]
        );
        let walk = w.walk().await.unwrap();
        assert_eq!(walk.heading, "CSR setup 2/2");
        assert!(
            button(&walk, "Suggest (now)"),
            "compacting on its own is opt-in"
        );
        let said = walk.lines.join(" ");
        assert!(said.contains(
            "Auto: on its own. A 20 s countdown at the terminal; at once from the phone"
        ));
        assert!(said.contains("181k–269k"));
        w.press("setup-autoCompact-1").await;
        assert_eq!(
            w.configured(),
            [
                json!({ "key": "csr-companion.dreams", "value": false }),
                json!({ "key": "csr-companion.autoCompact", "value": "auto" }),
            ]
        );
        assert!(w.walk().await.is_none(), "gone");
        assert_eq!(w.machine("setup:answer:dreams").unwrap()["value"], false);
        assert_eq!(
            w.machine("setup:answer:autoCompact").unwrap()["value"],
            "auto"
        );
        assert!(
            w.fx.host.seen.lock().unwrap().invalidated >= 2,
            "the band is drawn again"
        );
    }

    #[tokio::test]
    async fn keeping_what_is_in_force_writes_nothing_no_config_write_no_reload() {
        let w = first_run("s", false);
        w.start(start()).await;
        w.press("setup-dreams-1").await;
        w.press("setup-autoCompact-2").await;
        assert_eq!(w.configured(), Vec::<Value>::new());
        assert_eq!(
            w.machine("setup:answer:autoCompact").unwrap()["value"],
            "suggest"
        );
        assert!(w.walk().await.is_none());
    }

    #[tokio::test]
    async fn a_refused_config_row_says_where_to_set_it_and_moves_on_on_the_last_step_the() {
        let w = first_run("s", true);
        w.start(start()).await;
        w.press("setup-dreams-0").await;
        let walk = w.walk().await.unwrap();
        assert_eq!(walk.heading, "CSR setup 2/2");
        assert_eq!(
            walk.message.as_deref(),
            Some("Couldn't set it here. Use /config: csr-companion, Dreams.")
        );
        w.press("setup-autoCompact-1").await;
        assert!(w.walk().await.is_none());
        assert!(w.toasts().contains(
            &"Couldn't set it here. Use /config: csr-companion, Compact at the right moment."
                .to_string()
        ));
    }

    #[tokio::test]
    async fn answered_it_never_shows_again_csr_setup_brings_every_step_back() {
        let w = first_run("s", false);
        for step in STEPS {
            let at = json!({ "value": true, "at": 1 });
            w.fx.cx
                .store
                .set(MACHINE, &format!("setup:answer:{step}"), &at)
                .unwrap();
        }
        w.start(start()).await;
        assert!(w.walk().await.is_none());
        let said = run(
            &w.table,
            "csr",
            &w.fx.cx,
            json!({ "command": "csr", "args": "setup",
            "origin": { "kind": "human" } }),
        )
        .await;
        let text = said.answer.unwrap()["text"].as_str().unwrap().to_string();
        assert!(
            text.contains("CSR setup is above the prompt: 2 steps"),
            "{text}"
        );
        assert_eq!(w.heading().await.as_deref(), Some("CSR setup 1/2"));
    }

    #[tokio::test]
    async fn typing_sets_it_aside_for_the_session_a_lone_digit_on_its_way_to_the_band_does() {
        let w = first_run("s", false);
        w.start(start()).await;
        w.edit("1").await;
        assert_eq!(w.heading().await.as_deref(), Some("CSR setup 1/2"));
        w.edit("w").await;
        assert!(w.walk().await.is_none());
        // A reload of the module keeps it aside.
        w.start(start()).await;
        assert!(w.walk().await.is_none());
        assert!(view_of(&w.fx.cx).unwrap().aside);
    }

    #[tokio::test]
    async fn a_prompt_sent_past_it_sets_it_aside_too() {
        let w = first_run("s", false);
        w.start(start()).await;
        let sent = json!({ "text": "fix the importer", "origin": { "kind": "composer" } });
        run(&w.table, "digit", &w.fx.cx, sent).await;
        assert!(w.walk().await.is_none());
        // A prompt that is not a person's (a hook's own) leaves it up.
        let w = first_run("s2", false);
        w.start(start()).await;
        run(
            &w.table,
            "digit",
            &w.fx.cx,
            json!({ "text": "x", "origin": { "kind": "hook" } }),
        )
        .await;
        assert!(w.walk().await.is_some());
    }

    #[tokio::test]
    async fn set_aside_unanswered_at_2_session_starts_a_step_never_shows_again_one_start_in() {
        let w = first_run("sess-3", false);
        let store = &w.fx.cx.store;
        store
            .set(MACHINE, "setup:seen:dreams", &json!(["sess-1", "sess-2"]))
            .unwrap();
        store
            .set(MACHINE, "setup:seen:autoCompact", &json!(["sess-2"]))
            .unwrap();
        w.start(start()).await;
        // Dreams: two starts set it aside. Compacting: one, so a second chance.
        let shown = view_of(&w.fx.cx).unwrap();
        assert_eq!(shown.steps, ["autoCompact"]);
        assert_eq!(w.heading().await.as_deref(), Some("CSR setup 2/2"));
        assert_eq!(
            w.machine("setup:seen:autoCompact"),
            Some(json!(["sess-2", "sess-3"]))
        );
        assert_eq!(
            w.machine("setup:seen:dreams"),
            Some(json!(["sess-1", "sess-2"]))
        );
    }

    #[tokio::test]
    async fn a_claude_p_child_never_walks_through_anything() {
        let w = first_run("kid-3", false);
        w.start(json!({ "isInteractive": false, "surface": null }))
            .await;
        assert_eq!(w.machine("setup:seen:dreams"), None);
        assert_eq!(w.machine("setup:seen:autoCompact"), None);
        assert!(w.walk().await.is_none());
        assert!(view_of(&w.fx.cx).is_none());
    }

    // Not in the TypeScript's tests by name: the rest of what the module does.

    #[test]
    fn a_step_not_answered_shows_unless_set_aside_at_two_earlier_starts_and_a_reload_shows_it() {
        let seen = |step: &str| match step {
            "dreams" => vec!["a".to_string(), "b".to_string()],
            _ => vec!["a".to_string()],
        };
        let none = HashSet::new();
        assert_eq!(due_steps(&none, &seen, "c"), ["autoCompact"]);
        assert_eq!(due_steps(&none, &seen, "b"), ["dreams", "autoCompact"]);
        let answered: HashSet<&str> = ["autoCompact"].into();
        assert_eq!(due_steps(&answered, &seen, "b"), ["dreams"]);
    }

    #[test]
    fn a_press_key_names_one_choice_of_one_step() {
        assert_eq!(choice_of("setup-dreams-0").unwrap().1.value, json!(false));
        assert_eq!(
            choice_of("setup-autoCompact-1").unwrap().1.value,
            json!("auto")
        );
        assert_eq!(
            choice_of("setup-autoCompact-2").unwrap().1.value,
            json!("suggest")
        );
        assert!(choice_of("setup-autoCompact-3").is_none());
        assert!(choice_of("setup-dreams").is_none());
        assert!(choice_of("pane-open").is_none());
    }

    #[tokio::test]
    async fn a_press_for_a_step_the_band_does_not_show_writes_nothing() {
        let w = first_run("s", false);
        w.start(start()).await;
        w.press("setup-autoCompact-1").await; // dreams is the step in force
        w.press("setup-dreams-9").await;
        assert_eq!(w.configured(), Vec::<Value>::new());
        assert_eq!(w.machine("setup:answer:autoCompact"), None);
        assert_eq!(w.heading().await.as_deref(), Some("CSR setup 1/2"));
    }

    #[test]
    fn a_deny_that_says_something_is_a_refusal() {
        assert!(state_denies(&json!({ "deny": "policy" })));
        assert!(state_denies(&json!({ "deny": { "why": 1 } })));
        assert!(!state_denies(&json!({ "deny": "" })));
        assert!(!state_denies(&json!({ "deny": null })));
        assert!(!state_denies(&json!({ "deny": false })));
        assert!(!state_denies(&json!(true)));
    }
}
