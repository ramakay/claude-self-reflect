//! The band's tests: the removed TypeScript companion's, one for one by name,
//! and the rest of what the band does. The two that need a dream on screen
//! (`the_dim_4_on_a_dream...`, `a_dream_waits_while_you_should_know_asks...`) live with the
//! dreamer's rig, `dreamer/tests.rs`, which has the dreamer to make the dream.

pub(crate) mod kit;

use super::*;
use crate::companion::{
    app::{
        compactor::tests::kit::{Kit, Setup, LANDED},
        state::{show, strings},
        touch::{self, Files, NoteSource},
    },
    relay::{
        dispatch::testing::{fixture_in, run, until, Fixture},
        host::fake::World,
    },
    store::MACHINE,
};
use kit::{band, draw, gear, press, press_key, row, Gear, CLAUDE_MD};
use std::sync::Mutex;

const NOW: f64 = 1_791_547_200_000.;
const EDITED: &str = "/repo/src/hooks/mod.rs";
/// What the pane reads as `HUMAN`: a prompt typed in the composer.
const HUMAN: &str = "composer";

fn note() -> Value {
    json!({
        "id": "shift:src/hooks/mod.rs:ca5655a841fb:d8fe3a96",
        "kind": "shift",
        "file": "src/hooks/mod.rs",
        "line": "`dispatch_hook` in mod.rs changed in ca5655a on 09-17, after your 09-14 session \
            worked on it.",
        "title": "Your 09-14 work on dispatch_hook has changed since",
        "text": "On 09-14 you asked “switch to main”. That session worked on `dispatch_hook`.",
        "sketch": ["09-14  ● your session", "09-17  ▲ ca5655a  dispatch_hook"],
        "symbol": "dispatch_hook",
        "oid": "ca5655a841fb",
        "conversation": "d8fe3a96-e6f8",
        "at": 1,
    })
}

fn term_note() -> Value {
    json!({
        "id": "term:quokka", "kind": "term", "file": "",
        "line": "You said “quokka” in 2 sessions and Claude looked it up each time. \
            Define it once?",
        "title": "Define “quokka” once",
        "text": "- quokka: said in 2 sessions, each time Claude looked it up (e.g. “call quokka \
            now”). Edit this line to say what you mean.",
        "sketch": [], "symbol": null, "oid": null, "conversation": "", "at": 1,
    })
}

/// A disk with nothing on it: an edit's file was not there.
struct Bare;

impl Files for Bare {
    fn stat(&self, _path: &str) -> Option<(bool, u64)> {
        None
    }

    fn read(&self, _path: &str) -> Option<String> {
        None
    }
}

struct Rig {
    fx: Fixture,
    table: Table,
    gear: Gear,
    /// The files the note mode was asked about.
    asked: Arc<Mutex<Vec<String>>>,
}

fn rig_in(world: World) -> Rig {
    let asked: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = asked.clone();
    let source: NoteSource = Arc::new(move |file, _sid, _payload| {
        seen.lock().unwrap().push(file);
        Box::pin(async { Some(json!({ "notes": [note()], "candidates": 1 })) })
    });
    let gear = gear();
    let mut table = Table::new();
    register_with(&mut table, gear.seams.clone());
    touch::register_with(&mut table, source, Arc::new(Bare));
    register_late(&mut table);
    Rig {
        fx: fixture_in(world, "sess-1"),
        table,
        gear,
        asked,
    }
}

fn rig() -> Rig {
    rig_in(World::default())
}

impl Rig {
    fn cx(&self) -> Ctx {
        self.fx.cx.clone()
    }

    async fn tool(&self, tool: &str, file: &str, agent: Option<&str>) {
        let mut e = json!({ "tool": tool, "file_path": file, "old_string": "a",
            "new_string": "b" });
        if let Some(agent) = agent {
            e["agentId"] = json!(agent);
        }
        run(&self.table, "tool", &self.fx.cx, e).await;
    }

    /// A call that raises the note, and the wait until the band holds it.
    async fn noted(&self, tool: &str) {
        self.tool(tool, EDITED, None).await;
        assert!(until(|| self.view().is_some()).await, "a note is up");
    }

    fn view(&self) -> Option<Value> {
        row(&self.fx, "view")
    }

    async fn band(&self) -> kit::Drawn {
        band(&self.table, &self.fx.cx).await
    }

    async fn press(&self, label: &str) {
        assert!(
            press(&self.table, &self.fx.cx, label).await,
            "a button {label}"
        );
    }

    async fn prompt(&self, text: &str, kind: &str) -> Reply {
        let e = json!({ "text": text, "origin": { "kind": kind } });
        run(&self.table, "digit", &self.fx.cx, e).await
    }

    fn toasts(&self) -> Vec<String> {
        self.fx.host.seen.lock().unwrap().toasts.clone()
    }

    fn filled(&self) -> Vec<String> {
        self.fx.host.seen.lock().unwrap().filled.clone()
    }

    fn answers(&self) -> Vec<String> {
        let list = row_of(&self.fx, MACHINE, "answers");
        let rows = list.as_array().cloned().unwrap_or_default();
        rows.iter()
            .map(|r| r["answer"].as_str().unwrap_or("").to_string())
            .collect()
    }

    fn clock(&self, ms: f64) {
        self.fx.clock.set(NOW + ms);
    }
}

fn row_of(fx: &Fixture, sid: &str, key: &str) -> Value {
    fx.cx.store.get(sid, key).unwrap().unwrap_or(Value::Null)
}

// ---------------------------------------------------------------- the TypeScript's tests

#[tokio::test]
async fn an_edit_to_a_file_csr_remembers_offers_one_note_learn_more_opens_it_chat_hands() {
    let r = rig();
    r.noted("Edit").await;
    let shown = r.band().await;
    assert!(shown.has("Heads up"));
    assert!(shown.has("changed in ca5655a"));
    assert!(shown.has_button("Learn more"));
    assert!(shown.has_button("Knew this already"));
    assert_eq!(r.asked.lock().unwrap().as_slice(), [EDITED]);

    r.press("Learn more").await;
    let explained = r.band().await;
    assert!(explained.has(&regex::escape(note()["title"].as_str().unwrap())));
    assert!(explained.has("0 model tokens"));

    r.press("Chat in main session").await;
    let filled = r.filled();
    assert_eq!(filled.len(), 1);
    assert!(filled[0].contains("Here is a note offered by CSR"));
    assert!(filled[0].contains(note()["line"].as_str().unwrap()));
    assert!(filled[0].contains("commit ca5655a"));
    assert!(!r.band().await.has("changed in ca5655a"));
    assert_eq!(r.answers(), ["shown", "chat"]);
}

#[tokio::test]
async fn a_note_is_shown_once_knew_this_already_keeps_it_away_for_good() {
    let r = rig();
    r.noted("Read").await;
    assert!(r.band().await.has("You should know"));

    r.press("Knew this already").await;
    let id = note()["id"].as_str().unwrap().to_string();
    assert_eq!(strings(&r.cx(), MACHINE, "known"), [id]);
    assert!(!r.band().await.has("changed in ca5655a"));

    r.tool("Read", "/repo/other/mod.rs", None).await;
    let asked = r.asked.clone();
    assert!(until(|| asked.lock().unwrap().len() >= 2).await);
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert!(!r.band().await.has("changed in ca5655a"));
}

#[tokio::test]
async fn a_subagent_reading_the_file_raises_no_note_and_an_unanswered_note_clears_after() {
    let r = rig();
    r.tool("Read", EDITED, Some("sub-1")).await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert!(r.asked.lock().unwrap().is_empty());

    r.noted("Read").await;
    r.prompt("one", "human").await;
    assert!(r.band().await.has("changed in ca5655a"));
    r.prompt("two", "human").await;
    assert!(!r.band().await.has("changed in ca5655a"));
    assert_eq!(r.answers(), ["shown", "ignored"]);
}

#[tokio::test]
async fn a_bare_digit_just_after_a_band_note_is_held_with_a_toast_the_same_digit_again() {
    let r = rig();
    r.noted("Read").await;
    assert!(is_bare_digit(" 2 "));

    r.clock(2_000.);
    let first = r.prompt("2", HUMAN).await;
    let said = first.answer.unwrap()["drop"].as_str().unwrap().to_string();
    assert!(said.contains("looks like a band hotkey"));
    assert!(said.contains("send it again within 10 s"));
    assert_eq!(r.toasts(), [said]);

    r.clock(8_000.); // 6 s later: the same digit goes through
    let again = r.prompt("2", HUMAN).await;
    assert!(again.answer.is_none());

    r.clock(9_000.); // the held one is spent: a third is held again
    assert!(r.prompt("2", HUMAN).await.answer.is_some());
    assert_eq!(r.answers(), ["shown"], "nothing counted as typed past");
}

#[tokio::test]
async fn a_digit_sent_again_after_10_s_is_held_again_more_than_a_digit_or_no_band_or_30_s() {
    let r = rig();
    let plain = r.prompt("2", HUMAN).await; // no band yet
    assert!(plain.answer.is_none());

    r.noted("Read").await;
    r.clock(1_000.);
    assert!(r.prompt("2", HUMAN).await.answer.is_some());
    r.clock(12_000.); // past the 10 s window
    assert!(r.prompt("2", HUMAN).await.answer.is_some());
    for text in ["22", "2 please", "yes", ""] {
        assert!(r.prompt(text, HUMAN).await.answer.is_none(), "{text:?}");
    }
    r.clock(31_000.); // the band showed over 30 s ago
    assert!(r.prompt("3", HUMAN).await.answer.is_none());
}

/// Both bands up at once: a countdown, then the walkthrough under it, from the removed TypeScript
/// tests.
#[tokio::test]
async fn a_countdown_wins_over_the_setup_walkthrough_which_comes_back_once_the_countdown() {
    let kit = Kit::new(Setup::auto(214_000.));
    let mut table = Table::new();
    let seams = Seams {
        compact: kit.out.clone(),
        ..gear().seams
    };
    register_with(&mut table, seams);
    setup_walk::start_setup(&kit.cx).unwrap();
    let first = band(&table, &kit.cx).await;
    assert!(first.has("CSR setup 1/2"));

    kit.turn(LANDED, &[]).await;
    let counting = band(&table, &kit.cx).await;
    assert!(counting.has("Compacting in 20s"));
    assert!(!counting.has("CSR setup"));

    press_key(&table, &kit.cx, "compact-cancel").await;
    assert!(band(&table, &kit.cx).await.has("CSR setup 1/2"));
}

// ---------------------------------------------------------------- the rest of the band

#[tokio::test]
async fn a_digit_that_is_not_the_person_s_own_is_never_held() {
    let r = rig();
    r.noted("Read").await;
    for kind in ["human", "agent", "hook"] {
        assert!(r.prompt("2", kind).await.answer.is_none(), "{kind}");
    }
    assert!(r.toasts().is_empty());
}

#[tokio::test]
async fn a_digit_is_held_only_for_30_s_after_the_note_showed() {
    let r = rig();
    r.noted("Read").await;
    r.clock(30_000.);
    assert!(r.prompt("2", HUMAN).await.answer.is_some());
    let r = rig();
    r.noted("Read").await;
    r.clock(30_001.);
    assert!(r.prompt("2", HUMAN).await.answer.is_none());
}

#[tokio::test]
async fn a_question_from_another_band_holds_a_digit_though_no_note_is_up() {
    let r = rig();
    let asking = json!({ "type": "Box", "props": {}, "children": [
        { "type": "Button", "props": { "key": "ysk-disable", "hotkey": "4", "label": "Disable" },
            "children": [] }] });
    assert!(draw(&r.table, &r.cx(), asking).await.is_none());
    r.clock(5_000.);
    assert!(r.prompt("4", HUMAN).await.answer.is_some());
    r.clock(31_000.);
    assert!(r.prompt("4", HUMAN).await.answer.is_none());
}

#[tokio::test]
async fn a_note_waits_under_a_band_that_asks_and_prompts_meanwhile_are_not_typed_past_it() {
    let r = rig();
    r.noted("Read").await;
    let asking = json!({ "type": "Button", "props": { "key": "ysk-yes", "hotkey": "1",
        "label": "Yes" }, "children": [] });
    assert!(draw(&r.table, &r.cx(), asking.clone()).await.is_none());
    r.prompt("one", "human").await;
    r.prompt("two", "human").await;
    r.prompt("three", "human").await;
    assert_eq!(r.view().unwrap()["promptsSurvived"], 0);
    assert_eq!(r.answers(), ["shown"]);

    // Below a band that does not ask, the note stands above it.
    let quiet = json!({ "type": "Text", "props": {}, "children": ["below"] });
    let tree = draw(&r.table, &r.cx(), quiet).await.unwrap();
    assert_eq!(tree["children"][1], "$beneath");
}

#[tokio::test]
async fn the_first_prompt_a_note_survives_clears_its_message_and_counts_it() {
    let r = rig_in(World {
        prompt: "half a draft".into(),
        ..World::default()
    });
    r.noted("Read").await;
    r.press("Learn more").await;
    r.press("Chat in main session").await;
    let view = r.view().unwrap();
    assert!(view["message"].as_str().unwrap().contains("has text in it"));
    assert!(r.band().await.has("has text in it"));
    assert!(r.filled().is_empty(), "nothing was put over the draft");
    assert_eq!(view["kind"], "explained");

    r.prompt("one", "human").await;
    let view = r.view().unwrap();
    assert_eq!(view["promptsSurvived"], 1);
    assert!(view.get("message").is_none());
}

#[tokio::test]
async fn a_prompt_box_that_does_not_take_the_note_says_so_and_keeps_it() {
    let mut world = World::default();
    let fill = world.script.entry("prompt.fill".into()).or_default();
    fill.push_back(Ok(json!({ "isFilled": false })));
    let r = rig_in(world);
    r.noted("Read").await;
    r.press("Learn more").await;
    r.press("Chat in main session").await;
    assert!(r.band().await.has("didn't take it"));
    assert_eq!(r.view().unwrap()["kind"], "explained");
    assert_eq!(r.answers(), ["shown"]);
}

#[tokio::test]
async fn understood_and_dismiss_leave_a_question_and_its_answers_close_it() {
    let r = rig();
    r.noted("Read").await;
    r.press("Dismiss").await;
    let asked = r.band().await;
    assert!(asked.says("Was that worth showing?"));
    assert_eq!(
        asked.labels(),
        ["That was helpful", "Not relevant", "Close"]
    );
    r.press("Not relevant").await;
    assert!(r.view().is_none());
    assert_eq!(r.answers(), ["shown", "dismiss", "not_relevant"]);

    // The note is shown once; a later one may take the place of the question.
    let r = rig();
    r.noted("Read").await;
    r.press("Learn more").await;
    r.press("Understood").await;
    assert!(r.band().await.says("Was that worth showing?"));
    r.press("That was helpful").await;
    assert!(r.view().is_none());
    assert_eq!(r.answers(), ["shown", "got_it", "helpful"]);

    let r = rig();
    r.noted("Read").await;
    r.press("Dismiss").await;
    r.press("Close").await;
    assert!(r.view().is_none());
    assert_eq!(r.answers(), ["shown", "dismiss"], "Close answers nothing");
}

#[tokio::test]
async fn a_note_that_is_not_a_dream_has_no_disable_and_a_late_press_is_not_one() {
    let r = rig();
    r.noted("Read").await;
    assert!(!r.band().await.has_button("Disable"));
    press_key(&r.table, &r.cx(), "band-disable").await;
    press_key(&r.table, &r.cx(), "band-understood").await;
    press_key(&r.table, &r.cx(), "no-such-key").await;
    assert_eq!(r.view().unwrap()["kind"], "offer");
    assert!(r.fx.host.seen.lock().unwrap().configured.is_empty());

    press_key(&r.table, &r.cx(), "band-know").await;
    assert!(r.view().is_none());
    press_key(&r.table, &r.cx(), "band-know").await; // gone: nothing to answer
    assert_eq!(r.answers(), ["shown", "know_it"]);
}

#[tokio::test]
async fn open_the_csr_pane_opens_it_and_leaves_the_note_as_it_was() {
    let r = rig();
    r.noted("Read").await;
    r.press("Learn more").await;
    r.press("Open the CSR pane").await;
    let opened = r.fx.host.seen.lock().unwrap().opened.clone();
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0]["id"], "csr");
    assert!(
        r.gear.panes.ran.lock().unwrap().is_empty(),
        "no herdr split"
    );
    assert_eq!(r.view().unwrap()["kind"], "explained");
}

#[tokio::test]
async fn a_survey_on_screen_leaves_the_band_to_those_below() {
    let r = rig();
    r.noted("Read").await;
    let e = json!({ "component": "AbovePrompt", "props": { "hasSurvey": true }, "beneath": null });
    assert!(run(&r.table, "band", &r.cx(), e).await.tree.is_none());
    assert!(r.band().await.drew);
}

#[tokio::test]
async fn the_band_draws_nothing_when_there_is_no_note_and_nothing_to_walk_through() {
    let r = rig();
    assert!(!r.band().await.drew);
}

// ---------------------------------------------------------------- the coaching note

async fn coached(r: &Rig) {
    show(&r.cx(), &term_note(), "Coaching").unwrap();
    assert!(r.band().await.has("You said “quokka”"));
}

#[tokio::test]
async fn the_coaching_note_writes_one_line_under_terms_only_when_the_person_presses_1() {
    let r = rig();
    r.gear
        .machine
        .put(CLAUDE_MD, "# Prefs\n\n## Core\n- be brief\n");
    coached(&r).await;
    let shown = r.band().await;
    assert_eq!(shown.labels(), ["Add to CLAUDE.md", "Dismiss"]);
    assert!(r.gear.machine.writes().is_empty(), "shown is not written");

    r.press("Add to CLAUDE.md").await;
    let want = format!(
        "# Prefs\n\n## Core\n- be brief\n\n## Terms\n{}\n",
        term_note()["text"].as_str().unwrap()
    );
    assert_eq!(r.gear.machine.text(CLAUDE_MD), Some(want));
    assert!(r.toasts().iter().any(|text| text.contains("CLAUDE.md")));
    assert!(r.view().is_none());
    assert_eq!(r.answers(), ["shown", "added"]);
}

#[tokio::test]
async fn dismissing_the_coaching_note_leaves_claude_md_alone() {
    let r = rig();
    r.gear.machine.put(CLAUDE_MD, "# Prefs\n");
    coached(&r).await;
    r.press("Dismiss").await;
    assert!(r.view().is_none());
    assert_eq!(r.gear.machine.text(CLAUDE_MD).as_deref(), Some("# Prefs\n"));
    assert!(r.gear.machine.writes().is_empty());
}

#[tokio::test]
async fn a_claude_md_that_cannot_be_written_keeps_the_coaching_note_and_says_so() {
    let r = rig();
    r.gear.machine.put(CLAUDE_MD, "# Prefs\n");
    *r.gear.machine.locked.lock().unwrap() = true;
    coached(&r).await;
    r.press("Add to CLAUDE.md").await;
    assert!(r.band().await.has("Couldn't write your CLAUDE.md"));
    assert_eq!(r.view().unwrap()["note"]["id"], "term:quokka");
    assert_eq!(r.gear.machine.text(CLAUDE_MD).as_deref(), Some("# Prefs\n"));
}
