//! The pace's tests: the TypeScript's, one for one, and the rest of what the module does.

use super::*;
use crate::companion::relay::{
    dispatch::testing::{fixture_in, run, Fixture},
    host::{fake::World, Clock},
};
use std::{sync::Mutex, time::Duration};

/// What one step of Claude's costs: the four counts of `usage()`.
const LOOK: f64 = 41_000.;
const SID: &str = "sess-1";
const BRIEF: &str = "Things the person built in other sessions that came up in this work:\n* \
    skill notes-sync: built over 2 sessions, latest 08-26 [session dc24d328]";

/// A whole number as the store gives it back.
fn num(x: f64) -> Value {
    json!(x as i64)
}

fn usage() -> Value {
    json!({ "input_tokens": 900, "output_tokens": 100, "cache_creation_input_tokens": 0,
        "cache_read_input_tokens": 40_000, "model": "opus" })
}

/// A fake Claude Code whose `/config` says dreams are `dreams` for the next 3000 reads (the
/// TypeScript tests' `ON` and `{ dreams: false }`).
fn world(dreams: bool, headless: bool) -> World {
    let mut world = World {
        headless,
        ..World::default()
    };
    let rows = json!([{ "key": "csr-companion.dreams", "label": "Dreams",
        "kind": "boolean", "value": dreams }]);
    let list = world.script.entry("config.list".into()).or_default();
    for _ in 0..3000 {
        list.push_back(Ok(rows.clone()));
    }
    world
}

type Kept = Arc<Mutex<Vec<(String, Value)>>>;

struct Rig {
    fx: Fixture,
    table: Table,
    /// The `--keep` records the entries wrote, with the cwd they were written for.
    kept: Kept,
    /// Where the main session's next step continues the turn (the test host's `mainNext`).
    next: Mutex<u32>,
}

fn rig_in(world: World, sid: &str) -> Rig {
    let kept: Kept = Arc::default();
    let seen = kept.clone();
    let keep: Keep = Arc::new(move |cwd, record| {
        seen.lock().unwrap().push((cwd, record));
        Box::pin(async {})
    });
    let mut table = Table::new();
    register_with(&mut table, keep.clone());
    let fx = fixture_in(world, sid);
    install_keep(&fx.cx, &keep);
    Rig {
        fx,
        table,
        kept,
        next: Mutex::new(0),
    }
}

fn rig() -> Rig {
    rig_in(world(true, false), SID)
}

impl Rig {
    fn cx_in(&self, sid: &str) -> Ctx {
        Ctx {
            sid: sid.into(),
            ..self.fx.cx.clone()
        }
    }

    fn records(&self) -> Vec<Value> {
        self.kept
            .lock()
            .unwrap()
            .iter()
            .map(|(_, r)| r.clone())
            .collect()
    }

    fn row(&self, sid: &str, key: &str) -> Value {
        self.fx
            .cx
            .store
            .get(sid, key)
            .unwrap()
            .unwrap_or(Value::Null)
    }

    /// The `/after` of every entry of `key`, in order, as the app runs them.
    async fn after(&self, key: &str, cx: &Ctx, e: Value, left: Option<Value>) {
        for entry in self.table.entries(key) {
            if let Some(after) = &entry.after {
                let own = Ctx {
                    left: left.clone(),
                    ..cx.clone()
                };
                after(own, e.clone()).await.unwrap();
            }
        }
    }

    async fn step_at(&self, cx: &Ctx, turn: &str, index: u32, agent: Option<&str>) {
        let mut e = json!({ "turnId": turn, "index": index, "model": "opus",
            "messageCount": 4 });
        if let Some(agent) = agent {
            e["agentId"] = json!(agent);
        }
        let result = json!({ "turnId": turn, "index": index, "answer": "", "toolUses": [],
            "stopReason": "end_turn", "usage": usage() });
        self.after("step", cx, json!({ "e": e, "result": result }), None)
            .await;
    }

    /// `n` steps of a turn (test file's `steps`): the main session's continue the turn's index
    /// where the last call stopped, `from` replays the indexes from there, a subagent's start
    /// at 0.
    async fn steps(&self, cx: &Ctx, n: u32, agent: Option<&str>, from: Option<u32>) {
        for k in 0..n {
            let index = match (from, agent) {
                (Some(from), _) => from + k,
                (None, Some(_)) => k,
                (None, None) => {
                    let mut next = self.next.lock().unwrap();
                    *next += 1;
                    *next - 1
                }
            };
            self.step_at(cx, "turn-1", index, agent).await;
        }
    }

    async fn complete(&self, cx: &Ctx, turn: &str, more: Value) {
        let mut e = json!({ "answer": "ok", "durationMs": 1, "isAborted": false,
            "turnId": turn, "reason": "answer" });
        e.as_object_mut()
            .unwrap()
            .extend(more.as_object().unwrap().clone());
        run(&self.table, "turn_complete", cx, e).await;
    }

    /// A compaction: the `/event` of the `chapter` gate, then its `/after` with `result`.
    async fn compact(&self, cx: &Ctx, e: Value, result: Value) {
        let before = run(&self.table, "chapter", cx, e).await;
        let left = before.left.map(|packed| packed[0][1].clone());
        self.after("chapter", cx, result, left).await;
    }

    async fn start(&self, cx: &Ctx, interactive: bool) {
        let e = json!({ "cwd": "/repo", "isInteractive": interactive, "startedAt": 0 });
        run(&self.table, "start", cx, e).await;
    }
}

fn now(r: &Rig) -> f64 {
    r.fx.clock.now()
}

fn chapter_json(session: &str, started: f64, steps: u32) -> Value {
    json!({ "session": session, "started": started, "steps": steps, "due": 6, "looks": 0,
        "shown": 0, "adopted": 0 })
}

fn pace_json(session: &str, claude: u32) -> Value {
    json!({ "session": session, "claude": claude, "dreamer": 0, "looks": 0, "dreams": 0,
        "lastLook": 0 })
}

fn put(r: &Rig, sid: &str, key: &str, value: Value) {
    r.fx.cx.store.set(sid, key, &value).unwrap();
}

fn at(claude: f64, dreamer: f64, last_look: f64) -> PaceRow {
    PaceRow {
        claude,
        dreamer,
        last_look,
        ..PaceRow::fresh("s")
    }
}

// ---- the TypeScript's tests, one for one (companion.test.ts), by name.
//
// Where a test also asserts what the dreamer does (its forks, `look` records, the band), the
// pace's half is here and the dreamer's is left to the dreamer package: each such test says
// which assertions it leaves out.

/// Leaves out (the dreamer's): the `look`/`budget` record at step 6, the single fork at 12 and
/// its brief, the band's dream and its buttons, and `pace: { looks: 1, dreams: 1, dreamer }`.
#[tokio::test]
async fn on_the_first_look_waits_until_claude_has_spent_10_tokens_for_each_one_it_costs() {
    let r = rig();
    let cx = &r.fx.cx;
    r.steps(cx, 6, None, None).await; // 6 steps of Claude's
    let records = r.records();
    assert_eq!(records.len(), 1, "{records:?}");
    for (field, want) in [("kind", json!("chapter")), ("why", json!("start"))] {
        assert_eq!(records[0][field], want);
    }
    assert_eq!(records[0]["left"], json!(5));
    assert_eq!(records[0]["of"], json!(5));
    assert_eq!(records[0]["session"], json!(SID));
    assert!(fresh(cx), "the dreamer's step entry sees a new step");
    assert_eq!(counted_chapter(cx).unwrap().steps, 6.);
    r.steps(cx, 12, Some("sub-1"), None).await; // a subagent's 12: no spend, no step
    assert!(!fresh(cx), "a subagent's step is not one of the chapter's");
    assert_eq!(r.row(SID, "chapter")["steps"], json!(6));
    assert_eq!(r.row(SID, "pace")["claude"], num(6. * LOOK));
    r.steps(cx, 6, None, None).await; // 12 steps
    assert_eq!(r.row(SID, "pace")["claude"], num(12. * LOOK));
    assert_eq!(r.row(SID, "chapter")["steps"], json!(12));
    assert_eq!(r.records().len(), 1, "one chapter began, none after");
}

#[test]
fn only_the_brief_s_lines_about_the_work_in_hand_count_as_news() {
    let brief = [
        "Things the person built in other sessions that came up in this work:",
        "* skill chartkit: built over 2 sessions, latest 10-09 [session 5c1d0e77]",
        "  “Chart styles for reports”",
        "Lines the person wrote in past sessions that sit close to this work:",
        "- 06-10 (csr-engine): “the long-term plan” [session 9b2e4a10]",
        "Dreams already offered here, and what the person answered:",
        "- 10-09: “What if…?” [ask_claude]",
    ]
    .join("\n");
    assert_eq!(
        facts(&brief),
        ["* skill chartkit: built over 2 sessions, latest 10-09 [session 5c1d0e77]"]
    );
    // A line twice counts once, and a line is news only when the last look did not have it.
    assert_eq!(facts(&format!("{BRIEF}\n{BRIEF}")), facts(BRIEF));
    assert!(has_news(&facts(BRIEF), &[]));
    assert!(!has_news(&facts(BRIEF), &facts(BRIEF)));
}

#[test]
fn the_dreams_left_are_spread_over_the_rest_of_a_usual_chapter() {
    let chapter = Chapter {
        steps: 12.,
        shown: 1.,
        ..Chapter::fresh("s", 0.)
    };
    assert_eq!(spacing(&chapter, 120.), 27.);
    let long = Chapter {
        steps: 200.,
        ..chapter.clone()
    };
    assert_eq!(spacing(&long, 120.), 6., "a long chapter: no less than 6");
    let spent = Chapter {
        shown: 5.,
        ..chapter.clone()
    };
    assert_eq!(spacing(&spent, 120.), 0., "none left");
    let adopted = Chapter {
        adopted: 1.,
        ..spent
    };
    assert_eq!(spacing(&adopted, 120.), 108., "one adopted: its place back");
    assert_eq!(median(&[90., 150., 120.]), 120.);
    assert_eq!(median(&[]), 120.);
}

/// Leaves out (the dreamer's): the step-6 `look` records of each session (`budget`), and the
/// pace rows' `looks`.
#[tokio::test]
async fn two_sessions_sharing_the_store_keep_their_own_chapters_and_pace_no_restart_no() {
    let r = rig();
    let (one, two) = (r.cx_in("sess-1"), r.cx_in("sess-2"));
    r.steps(&one, 4, None, None).await; // sess-1: 4 steps
    r.steps(&two, 4, None, None).await; // another window: 4 steps of its own
    r.steps(&one, 4, None, None).await; // sess-1 again: 8 steps, never restarted
    r.steps(&two, 4, None, None).await;
    r.steps(&one, 4, None, None).await; // 12
    let starts: Vec<Value> = (r.records().into_iter())
        .filter(|record| record["kind"] == "chapter" && record["why"] == "start")
        .map(|record| record["session"].clone())
        .collect();
    assert_eq!(starts, [json!("sess-1"), json!("sess-2")]);
    assert_eq!(r.row("sess-1", "chapter")["session"], json!("sess-1"));
    assert_eq!(r.row("sess-1", "chapter")["steps"], json!(12));
    assert_eq!(r.row("sess-2", "chapter")["session"], json!("sess-2"));
    assert_eq!(r.row("sess-2", "chapter")["steps"], json!(8));
    assert_eq!(r.row("sess-1", "pace")["claude"], num(12. * LOOK));
    assert_eq!(r.row("sess-2", "pace")["claude"], num(8. * LOOK));
    // Nothing unkeyed for another process to take.
    assert_eq!(r.row(MACHINE, "chapter"), Value::Null);
    assert_eq!(r.row(MACHINE, "pace"), Value::Null);
}

#[tokio::test]
async fn a_claude_p_child_that_loads_the_plugin_never_counts_a_step_starts_a_chapter_or() {
    let r = rig_in(world(true, true), "kid-1");
    let cx = &r.fx.cx;
    r.fx.cx.session.lock().unwrap().interactive = Some(false);
    r.start(cx, false).await;
    r.steps(cx, 30, None, None).await;
    r.compact(
        cx,
        json!({ "trigger": "auto", "messages": [] }),
        json!({ "messages": [] }),
    )
    .await;
    assert_eq!(r.records(), Vec::<Value>::new());
    let rows = r.fx.cx.store.keys("kid-1", "").unwrap();
    assert!(
        rows.iter().all(|key| key != "pace" && key != "chapter"),
        "{rows:?}"
    );
    assert!(!fresh(cx));
}

#[tokio::test]
async fn a_child_loaded_without_a_session_start_is_told_apart_by_where_it_draws_nowhere() {
    let r = rig_in(world(true, true), "kid-2");
    let cx = &r.fx.cx;
    r.fx.cx.session.lock().unwrap().interactive = None; // no session.start was heard
    r.steps(cx, 12, None, None).await;
    assert_eq!(r.records(), Vec::<Value>::new());
    assert_eq!(r.row("kid-2", "pace"), Value::Null);
    let asked =
        r.fx.host
            .calls()
            .iter()
            .filter(|(m, _)| m == "session.surfaces")
            .count();
    assert_eq!(asked, 1, "asked once, the answer kept");
}

#[tokio::test]
async fn a_session_start_sweeps_the_unkeyed_keys_and_all_but_the_newest_sessions() {
    let r = rig();
    let cx = &r.fx.cx;
    let t = now(&r);
    put(&r, MACHINE, "chapter", chapter_json("kid-0", t, 1));
    put(&r, MACHINE, "pace", pace_json("kid-0", 32_346));
    for i in 0..22 {
        let started = t - f64::from(i + 1) * 3_600_000.;
        put(
            &r,
            &format!("old-{i}"),
            "chapter",
            chapter_json(&format!("old-{i}"), started, 1),
        );
        put(
            &r,
            &format!("old-{i}"),
            "pace",
            pace_json(&format!("old-{i}"), 1),
        );
    }
    put(&r, MACHINE, "seenDreams", json!(["What if?"]));
    r.start(cx, true).await;
    assert_eq!(r.row(MACHINE, "pace"), Value::Null);
    assert_eq!(r.row(MACHINE, "chapter"), Value::Null);
    assert_ne!(r.row("old-0", "chapter"), Value::Null, "newest 20 kept");
    assert_ne!(r.row("old-19", "chapter"), Value::Null);
    assert_eq!(r.row("old-20", "chapter"), Value::Null);
    assert_eq!(r.row("old-21", "pace"), Value::Null);
    assert_eq!(
        r.row(MACHINE, "seenDreams"),
        json!(["What if?"]),
        "shared lists stay"
    );
}

#[tokio::test]
async fn a_session_start_never_sweeps_a_session_whose_chapter_it_has_not_seen_only_known() {
    let r = rig();
    let cx = &r.fx.cx;
    let t = now(&r);
    put(&r, "other-live", "asked", json!(["keep the import"]));
    put(&r, "other-live", "pace", pace_json("other-live", 41_000));
    for i in 0..22 {
        let started = t - f64::from(i + 1) * 3_600_000.;
        put(
            &r,
            &format!("old-{i}"),
            "chapter",
            chapter_json(&format!("old-{i}"), started, 1),
        );
        put(&r, &format!("old-{i}"), "asked", json!(["x"]));
    }
    r.start(cx, true).await;
    // Known and past the newest 20: swept, its other keys too.
    assert_eq!(r.row("old-20", "chapter"), Value::Null);
    assert_eq!(r.row("old-20", "asked"), Value::Null);
    assert_eq!(r.row("old-21", "asked"), Value::Null);
    assert_eq!(r.row("other-live", "asked"), json!(["keep the import"]));
    assert_eq!(r.row("other-live", "pace")["claude"], json!(41_000));
}

/// Leaves out (the dreamer's): the single `look` record `spent` after 60 steps, and the fork
/// at step 12 of the next chapter.
#[tokio::test]
async fn a_chapter_holds_five_dreams_with_none_left_there_is_no_look_until_a_compaction() {
    let r = rig();
    let cx = &r.fx.cx;
    let spent = json!({ "session": SID, "started": now(&r), "steps": 0, "due": 6,
        "looks": 5, "shown": 5, "adopted": 0 });
    put(&r, SID, "chapter", spent);
    r.steps(cx, 60, None, None).await;
    assert_eq!(r.row(SID, "chapter")["steps"], json!(60));
    assert_eq!(
        r.records(),
        Vec::<Value>::new(),
        "a chapter was running: none began"
    );
    let done = json!({ "messages": [] });
    r.compact(cx, json!({ "trigger": "auto", "messages": [] }), done)
        .await;
    let records = r.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["kind"], json!("chapter"));
    assert_eq!(records[0]["why"], json!("compact"));
    assert_eq!(
        (records[0]["left"].clone(), records[0]["of"].clone()),
        (json!(5), json!(5))
    );
    // How long your chapters run, learned.
    assert_eq!(r.row(MACHINE, "chapterSteps"), json!([60]));
    assert_eq!(usual_steps(cx), 60.);
    let fresh_chapter = r.row(SID, "chapter");
    assert_eq!(
        (
            fresh_chapter["steps"].clone(),
            fresh_chapter["shown"].clone()
        ),
        (json!(0), json!(0))
    );
    assert_eq!(fresh_chapter["gen"], json!(1));
    r.steps(cx, 12, None, None).await;
    assert_eq!(r.row(SID, "chapter")["steps"], json!(12));
}

/// The look's spend and Claude's steps interleave, as the TypeScript's gated store write made
/// them: here the look is inside its change, holding what it read, when the steps count. The
/// pace's store is not asynchronous as the plugin's was, so the look sits in its closure.
/// Leaves out (the dreamer's): the look itself, its fork and its `none` record.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_look_s_spend_and_claude_s_steps_both_land_when_they_interleave_pace_changes_go() {
    let r = rig();
    let cx = r.fx.cx.clone();
    r.steps(&cx, 12, None, None).await;
    let spend = |pace: PaceRow| {
        std::thread::sleep(Duration::from_millis(150));
        PaceRow {
            looks: pace.looks + 1.,
            dreamer: pace.dreamer + LOOK,
            last_look: LOOK,
            ..pace
        }
    };
    let look = tokio::spawn({
        let cx = cx.clone();
        async move { change_pace(&cx, spend, None).await }
    });
    tokio::time::sleep(Duration::from_millis(40)).await; // the look has read and is working
    let going = r.steps(&cx, 3, None, None); // Claude's next steps count theirs
    let (done, ()) = tokio::join!(look, going);
    done.unwrap().unwrap();
    let pace = r.row(SID, "pace");
    assert_eq!(pace["claude"], num(15. * LOOK));
    assert_eq!(pace["dreamer"], num(LOOK));
    assert_eq!(pace["looks"], json!(1));
    assert_eq!(pace["lastLook"], num(LOOK));
    assert_eq!(r.row(SID, "chapter")["steps"], json!(15));
}

#[test]
fn the_share_owed_is_what_the_dreamer_spent_past_1_token_in_10_and_nothing_runs() {
    assert_eq!(owed(&at(1_000., 100., 0.)), 0.);
    assert_eq!(owed(&at(1_000., 160., 0.)).round(), 60.);
    assert!(
        can_afford(&at(1_000., 0., 0.), 100.),
        "100 more is exactly the share"
    );
    assert!(
        !can_afford(&at(1_000., 0., 0.), 101.),
        "the guess alone is over it"
    );
    assert!(
        can_afford(&at(1_000., 60., 40.), 0.),
        "the last look's cost is the guess"
    );
    assert!(!can_afford(&at(1_000., 60., 41.), 0.));
    // Over the share (a look that cost more than guessed): even a guess of 0 does not run one.
    assert!(!can_afford(&at(1_000., 160., 0.), 0.));
    // Claude's spending is what pays it back.
    assert!(!can_afford(&at(1_600., 160., 160.), 0.), "320 > 160");
    assert!(can_afford(&at(3_200., 160., 160.), 0.), "320 <= 320");
}

#[test]
fn the_pane_says_when_a_look_ran_over_its_share_and_does_not_claim_never() {
    let over = PaceRow {
        claude: 100_000.,
        dreamer: 160_000.,
        looks: 1.,
        last_look: 160_000.,
        ..PaceRow::fresh("s")
    };
    assert!(pace_text(Some(&over), None, true).contains(
        "A look ran over its share by 150k; the next waits until Claude's spending has \
         caught up."
    ));
    let paid = PaceRow {
        claude: 2_000_000.,
        ..over
    };
    assert!(!pace_text(Some(&paid), None, true).contains("ran over"));
    let none = pace_text(None, None, true);
    assert!(none.contains(
        "checks before each look that its spend stays within 1 token for every 10 Claude \
         spends"
    ));
    assert!(!none.contains("never spends") && !none.contains("at most"));
}

/// The pace's half: what an overshooting look leaves, and when the gate opens again. The look
/// that runs and is refused (`budget`, `quiet` records, the fork count) is the dreamer's.
#[tokio::test]
async fn an_overshooting_look_is_owed_the_next_looks_are_refused_until_claude_has_spent() {
    let r = rig();
    let cx = &r.fx.cx;
    let big = json!({ "input_tokens": 900, "output_tokens": 100,
        "cache_creation_input_tokens": 0, "cache_read_input_tokens": 5. * LOOK - 1_000. });
    r.steps(cx, 12, None, None).await; // the first look is within the share by its guess
    let used = tokens_of(&big);
    assert_eq!(used, 5. * LOOK, "a look that costs 5 steps of Claude's");
    // What `look` writes once the fork returns (register.tsx 762).
    let spend = |pace: PaceRow| PaceRow {
        looks: pace.looks + 1.,
        dreamer: pace.dreamer + used,
        last_look: if used != 0. { used } else { pace.last_look },
        ..pace
    };
    let pace = change_pace(cx, spend, None).await.unwrap();
    assert_eq!(
        [pace.claude, pace.dreamer, pace.looks, pace.last_look],
        [12. * LOOK, 5. * LOOK, 1., 5. * LOOK]
    );
    assert_eq!(r.row(SID, "pace")["dreamer"], num(5. * LOOK));
    assert_eq!(
        owed(&pace).round(),
        (5. * LOOK - 1.2 * LOOK).round(),
        "205k against 49.2k"
    );
    assert!(!can_afford(&pace, LOOK));
    r.steps(cx, 28, None, None).await; // step 40: still over the share
    let pace = pace_of(cx);
    assert_eq!(pace.claude, 40. * LOOK);
    assert!(owed(&pace) > 0. && !can_afford(&pace, LOOK));
    // Claude spends on: the total and the guess fit again, and the gate opens.
    let mut row = r.row(SID, "pace");
    row["claude"] = num(100. * LOOK);
    put(&r, SID, "pace", row);
    let pace = pace_of(cx);
    assert_eq!(owed(&pace), 0.);
    assert!(can_afford(&pace, LOOK));
}

/// The pace's half: a spend is written down whatever happens to the look after it (it passes
/// no `held`), and a change a lost lease holds back is not made. That `look` writes it before
/// it asks whether it still owns the lease is the dreamer's.
#[tokio::test]
async fn a_fork_is_paid_for_even_when_its_look_is_dropped_a_lost_lease_does_not_lose_the() {
    let r = rig();
    let cx = &r.fx.cx;
    r.steps(cx, 12, None, None).await;
    let lost = || false; // another copy holds the lease now
    let spend = |pace: PaceRow| PaceRow {
        looks: pace.looks + 1.,
        dreamer: pace.dreamer + 5. * LOOK,
        last_look: 5. * LOOK,
        ..pace
    };
    let held_back = change_pace(cx, spend, Some(&lost)).await.unwrap();
    assert_eq!(
        (held_back.looks, held_back.dreamer),
        (0., 0.),
        "a held change is not made"
    );
    change_pace(cx, spend, None).await.unwrap();
    let pace = r.row(SID, "pace");
    assert_eq!(pace["dreamer"], num(5. * LOOK));
    assert_eq!(pace["looks"], json!(1));
    assert_eq!(pace["lastLook"], num(5. * LOOK));
}

/// The pace's half: a fork that rejects is charged as the guess (`guess`), here Claude's last
/// step. Making the fork is the dreamer's.
#[tokio::test]
async fn a_fork_that_rejects_may_have_been_paid_for_it_is_charged_as_the_guess() {
    let r = rig();
    let cx = &r.fx.cx;
    r.steps(cx, 12, None, None).await;
    let last_step = pace_of(cx).step.unwrap();
    assert_eq!(last_step, LOOK);
    let charge = |pace: PaceRow| PaceRow {
        looks: pace.looks + 1.,
        dreamer: pace.dreamer + guess(&pace, last_step),
        ..pace
    };
    change_pace(cx, charge, None).await.unwrap();
    let pace = r.row(SID, "pace");
    assert_eq!(
        (pace["looks"].clone(), pace["dreamer"].clone()),
        (json!(1), num(LOOK))
    );
    // With a look behind it, that look's cost is the guess.
    // The guess is the last look's cost once there is one, else Claude's last step.
    let look = |pace: PaceRow| PaceRow {
        last_look: 3. * LOOK,
        ..pace
    };
    change_pace(cx, look, None).await.unwrap();
    let pace = change_pace(cx, charge, None).await.unwrap();
    assert_eq!(pace.dreamer, LOOK + 3. * LOOK, "now the last look's cost");
    assert_eq!(pace.looks, 2.);
}

#[tokio::test]
async fn agent_usage_never_counts_towards_the_share_or_the_chapter() {
    let r = rig();
    let cx = &r.fx.cx;
    r.steps(cx, 2, None, None).await;
    r.steps(cx, 3, Some("sub-1"), None).await; // the dreamer reads the main conversation
    r.steps(cx, 3, Some("sub-1"), None).await; // and a replay changes nothing either
    assert_eq!(r.row(SID, "pace")["claude"], num(2. * LOOK));
    assert_eq!(
        r.row(SID, "chapter")["steps"],
        json!(2),
        "only Claude's own steps count"
    );
    // A subagent's turn ends: not a close of the main session's turn.
    r.complete(
        cx,
        "turn-1",
        json!({ "agentId": "sub-1", "usage": usage() }),
    )
    .await;
    let pace = pace_of(cx);
    assert_eq!(pace.claude, 2. * LOOK);
    assert!(pace.turns.0.iter().all(|(_, receipt)| !receipt.closed));
    assert_eq!(
        r.records().len(),
        1,
        "the chapter's start, and no look of any kind"
    );
}

#[tokio::test]
async fn a_replayed_main_step_adds_no_usage_and_no_chapter_step_a_new_step_still_does() {
    let r = rig();
    let cx = &r.fx.cx;
    r.steps(cx, 3, None, None).await;
    r.steps(cx, 3, None, Some(0)).await; // the same three steps of the same turn: a replay
    assert!(!fresh(cx), "a replay is not new");
    let pace = r.row(SID, "pace");
    assert_eq!(
        (pace["claude"].clone(), pace["step"].clone()),
        (num(3. * LOOK), num(LOOK))
    );
    assert_eq!(r.row(SID, "chapter")["steps"], json!(3));
    r.steps(cx, 1, None, None).await; // the next index of the turn: new work
    assert!(fresh(cx));
    assert_eq!(r.row(SID, "pace")["claude"], num(4. * LOOK));
    assert_eq!(r.row(SID, "chapter")["steps"], json!(4));
}

#[tokio::test]
async fn main_step_receipts_an_open_turn_is_never_forgotten_a_closed_one_is_trimmed_and() {
    let r = rig();
    let cx = &r.fx.cx;
    r.step_at(cx, "open-0", 0, None).await; // a turn left open (no turn.complete) ...
    for n in 0..70 {
        let turn = format!("done-{n}");
        r.step_at(cx, &turn, 0, None).await;
        r.complete(cx, &turn, json!({})).await;
    }
    r.step_at(cx, "open-0", 0, None).await; // ... is still known after 70 closed turns
    assert_eq!(r.row(SID, "pace")["claude"], num(71. * LOOK));
    assert_eq!(r.row(SID, "chapter")["steps"], json!(71));
    let pace = pace_of(cx);
    assert!(
        pace.turns
            .0
            .iter()
            .filter(|(_, receipt)| receipt.closed)
            .count()
            <= 60
    );
    assert!(pace
        .turns
        .0
        .iter()
        .any(|(key, _)| *key == turn_key(SID, "open-0")));
    // ... and the same index of a new turn counts.
    r.step_at(cx, "done-0", 0, None).await;
    assert_eq!(r.row(SID, "pace")["claude"], num(72. * LOOK));
}

// ---- what the TypeScript's tests do not name: the rest of what the module does.

#[tokio::test]
async fn dreams_turned_off_in_config_count_nothing_and_start_no_chapter() {
    let r = rig_in(world(false, false), SID);
    let cx = &r.fx.cx;
    r.steps(cx, 30, None, None).await;
    assert!(!fresh(cx));
    assert_eq!(r.row(SID, "pace"), Value::Null);
    assert_eq!(r.row(SID, "chapter"), Value::Null);
    r.compact(cx, json!({ "trigger": "auto" }), json!({ "messages": [] }))
        .await;
    assert_eq!(r.records(), Vec::<Value>::new());
}

#[tokio::test]
async fn a_step_leaves_the_dreamer_whether_it_was_new_and_the_chapter_it_made() {
    let r = rig();
    let cx = &r.fx.cx;
    assert!(!fresh(cx) && counted_chapter(cx).is_none());
    r.steps(cx, 1, None, None).await;
    assert!(fresh(cx));
    let chapter = counted_chapter(cx).unwrap();
    assert_eq!(
        (chapter.steps, chapter.due, chapter.gen),
        (1., FIRST_EVERY, Some(1.))
    );
    r.steps(cx, 1, None, Some(0)).await; // a replay clears what the last step left
    assert!(!fresh(cx) && counted_chapter(cx).is_none());
}

#[tokio::test]
async fn a_turn_ends_closing_its_receipt_for_a_person_s_main_turn_with_dreams_on_only() {
    let r = rig();
    let cx = &r.fx.cx;
    r.steps(cx, 2, None, None).await;
    r.complete(cx, "turn-1", json!({ "reason": "interrupted" }))
        .await;
    assert!(
        pace_of(cx).turns.0[0].1.closed,
        "the receipt closes at any reason"
    );
    let off = rig_in(world(false, false), SID);
    off.fx
        .cx
        .store
        .set(SID, "pace", &{
            let mut pace = pace_json(SID, 1);
            pace["turns"] = json!([[turn_key(SID, "turn-1"), { "steps": [0] }]]);
            pace
        })
        .unwrap();
    off.complete(&off.fx.cx, "turn-1", json!({})).await;
    assert!(
        !pace_of(&off.fx.cx).turns.0[0].1.closed,
        "dreams off: untouched"
    );
    let child = rig();
    child.steps(&child.fx.cx, 1, None, None).await;
    child.fx.cx.session.lock().unwrap().interactive = Some(false);
    child.complete(&child.fx.cx, "turn-1", json!({})).await;
    assert!(
        !pace_of(&child.fx.cx).turns.0[0].1.closed,
        "not a person's: untouched"
    );
}

#[tokio::test]
async fn the_chapter_gate_starts_the_next_chapter_only_for_a_person_s_main_compaction() {
    let r = rig();
    let cx = &r.fx.cx;
    let done = json!({ "messages": [] });
    r.steps(cx, 3, None, None).await;
    let still = r.row(SID, "chapter");
    // A subagent's, a precompute, one a hook vetoed (`skip`): the chapter goes on.
    r.compact(
        cx,
        json!({ "trigger": "auto", "agentId": "a1" }),
        done.clone(),
    )
    .await;
    r.compact(cx, json!({ "trigger": "precompute" }), done.clone())
        .await;
    r.compact(
        cx,
        json!({ "trigger": "auto" }),
        json!({ "skip": "vetoed" }),
    )
    .await;
    assert_eq!(r.row(SID, "chapter"), still);
    r.fx.cx.session.lock().unwrap().interactive = Some(false);
    r.compact(cx, json!({ "trigger": "auto" }), done.clone())
        .await;
    assert_eq!(
        r.row(SID, "chapter"),
        still,
        "a child's compaction is not a chapter's end"
    );
    r.fx.cx.session.lock().unwrap().interactive = Some(true);
    r.fx.clock.advance(1_000.);
    r.compact(cx, json!({ "trigger": "manual" }), done).await;
    let chapter = r.row(SID, "chapter");
    assert_eq!(
        (chapter["steps"].clone(), chapter["gen"].clone()),
        (json!(0), json!(2))
    );
    let last = r.records().pop().unwrap();
    assert_eq!(
        (last["why"].clone(), last["interactive"].clone()),
        (json!("compact"), json!(true))
    );
    assert_eq!(last["at"], json!(now(&r) / 1000.));
}

#[tokio::test]
async fn a_chapter_two_hours_old_starts_over_and_a_new_session_starts_one() {
    let r = rig();
    let cx = &r.fx.cx;
    let first = chapter_of(cx).await.unwrap();
    assert_eq!((first.gen, first.started), (Some(1.), now(&r)));
    r.fx.clock.advance(CHAPTER_MS - 1.);
    assert_eq!(
        chapter_of(cx).await.unwrap(),
        first,
        "just short of two hours"
    );
    r.fx.clock.advance(1.);
    let second = chapter_of(cx).await.unwrap();
    assert_eq!((second.gen, second.started), (Some(2.), now(&r)));
    let why: Vec<Value> = r.records().iter().map(|r| r["why"].clone()).collect();
    assert_eq!(why, [json!("start"), json!("two hours")]);
    // A chapter row of another session's name is no chapter of this one.
    put(&r, SID, "chapter", chapter_json("someone-else", now(&r), 9));
    let third = chapter_of(cx).await.unwrap();
    assert_eq!(
        (third.session.as_str(), third.steps, third.gen),
        (SID, 0., Some(1.))
    );
}

#[tokio::test]
async fn the_chapter_lengths_the_machine_remembers_are_the_last_twenty() {
    let r = rig();
    let cx = &r.fx.cx;
    for n in 1..=22 {
        put(&r, SID, "chapter", chapter_json(SID, now(&r), n));
        new_chapter(cx, "compact").await.unwrap();
    }
    let lengths = r.row(MACHINE, "chapterSteps");
    assert_eq!(lengths.as_array().unwrap().len(), CHAPTERS_KEPT);
    assert_eq!(
        (lengths[0].clone(), lengths[19].clone()),
        (json!(3), json!(22))
    );
    assert_eq!(
        usual_steps(cx),
        13.,
        "the median of 3..=22, rounded half up"
    );
    // A chapter with no steps teaches nothing, and neither does a start.
    new_chapter(cx, "compact").await.unwrap();
    new_chapter(cx, "start").await.unwrap();
    assert_eq!(r.row(MACHINE, "chapterSteps"), lengths);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chapter_changes_go_through_one_queue() {
    let r = rig();
    let cx = r.fx.cx.clone();
    let tasks: Vec<_> = (0..6)
        .map(|_| {
            let cx = cx.clone();
            let more = |chapter: Chapter| {
                std::thread::sleep(Duration::from_millis(20));
                Chapter {
                    steps: chapter.steps + 1.,
                    ..chapter
                }
            };
            tokio::spawn(async move { change_chapter(&cx, more, None).await })
        })
        .collect();
    for task in tasks {
        task.await.unwrap().unwrap();
    }
    assert_eq!(r.row(SID, "chapter")["steps"], json!(6));
}

#[tokio::test]
async fn a_held_chapter_change_is_not_made_and_a_serial_new_chapter_waits_its_turn() {
    let r = rig();
    let cx = &r.fx.cx;
    r.steps(cx, 2, None, None).await;
    let lost = || false;
    let plus = |chapter: Chapter| Chapter {
        steps: 99.,
        ..chapter
    };
    let held = change_chapter(cx, plus, Some(&lost)).await.unwrap();
    assert_eq!(held.steps, 2.);
    assert_eq!(r.row(SID, "chapter")["steps"], json!(2));
    let next = new_chapter_serially(cx, "compact").await.unwrap();
    assert_eq!((next.steps, next.gen), (0., Some(2.)));
}

#[tokio::test]
async fn a_start_that_is_not_a_person_s_sweeps_nothing() {
    let r = rig();
    let cx = &r.fx.cx;
    put(&r, MACHINE, "pace", pace_json("kid-0", 1));
    r.start(cx, false).await;
    assert_ne!(r.row(MACHINE, "pace"), Value::Null);
    r.start(cx, true).await;
    assert_eq!(r.row(MACHINE, "pace"), Value::Null);
}

#[tokio::test]
async fn the_sweep_keeps_this_session_and_ties_stay_in_order() {
    let r = rig();
    let cx = &r.fx.cx;
    let same = now(&r);
    // 22 sessions that started at the same time, and this one among them.
    for i in 0..22 {
        let sid = format!("s{i:02}");
        put(&r, &sid, "chapter", chapter_json(&sid, same, 1));
        put(&r, &sid, "storyStarts", json!([1]));
    }
    put(&r, SID, "chapter", chapter_json(SID, 0., 1));
    r.start(cx, true).await;
    // The oldest by start go; of the equal ones the later names (sorted) go first.
    assert_ne!(r.row("s00", "chapter"), Value::Null);
    assert_ne!(r.row("s19", "chapter"), Value::Null);
    assert_eq!(r.row("s20", "chapter"), Value::Null);
    assert_eq!(r.row("s21", "chapter"), Value::Null);
    assert_ne!(
        r.row(SID, "chapter"),
        Value::Null,
        "this session is never swept"
    );
    // The TypeScript's pattern leaves `storyStarts` of old sessions where they are.
    assert_eq!(r.row("s20", "storyStarts"), json!([1]));
}

#[test]
fn a_pace_round_trips_with_what_it_does_not_know_and_the_order_of_its_turns() {
    let row = json!({ "session": "s", "claude": 41_000, "dreamer": 0, "looks": 0,
        "dreams": 0, "lastLook": 0, "step": 41_000, "later": { "x": [1, 2] },
        "turns": [["b", { "steps": [0, 1] }], ["a", { "steps": [0], "closed": true }]] });
    let pace: PaceRow = serde_json::from_value(row.clone()).unwrap();
    assert_eq!(pace.turns.0[0].0, "b", "order is what was written");
    assert_eq!(serde_json::to_value(&pace).unwrap(), row);
    // A row the TypeScript plugin wrote: turns as an object, a pace with fields left out.
    let old = json!({ "session": "s", "claude": 5, "turns": { "k": { "steps": [3] } } });
    let pace: PaceRow = serde_json::from_value(old).unwrap();
    assert_eq!((pace.claude, pace.looks, pace.step), (5., 0., None));
    assert_eq!(
        pace.turns.0,
        [(
            "k".to_string(),
            Receipt {
                steps: vec![3.],
                closed: false
            }
        )]
    );
    // Whole numbers are written whole, as `JSON.stringify` does.
    let text = serde_json::to_string(&PaceRow::fresh("s")).unwrap();
    assert!(
        text.contains("\"claude\":0,") && !text.contains("0.0"),
        "{text}"
    );
}

#[tokio::test]
async fn a_row_of_another_session_or_one_that_is_not_a_pace_reads_as_a_fresh_pace() {
    let r = rig();
    let cx = &r.fx.cx;
    put(&r, SID, "pace", pace_json("someone-else", 7));
    assert_eq!(pace_of(cx), PaceRow::fresh(SID));
    put(&r, SID, "pace", json!("nonsense"));
    assert_eq!(pace_of(cx), PaceRow::fresh(SID));
    put(&r, SID, "pace", json!({ "claude": 5 }));
    assert_eq!(
        pace_of(cx),
        PaceRow::fresh(SID),
        "no session: not this one's"
    );
}

#[test]
fn soon_spread_and_kept_move_the_next_look_as_the_look_leaves_the_chapter() {
    let chapter = Chapter {
        steps: 20.,
        ..Chapter::fresh("s", 0.)
    };
    assert_eq!(soon(20.)(chapter.clone()).due, 26.);
    assert_eq!(
        spread(12., 120.)(chapter.clone()).due,
        12. + 22.,
        "(120 - 12) / 5"
    );
    let spent = Chapter {
        shown: 5.,
        ..chapter.clone()
    };
    assert_eq!(spread(12., 120.)(spent.clone()).due, NEVER);
    assert_eq!(kept(spent).due, NEVER);
    assert_eq!(kept(chapter.clone()), chapter);
    let sides = sides_of(&at(1_000., 160., 40.), 7.);
    assert_eq!(
        sides,
        json!({ "claude": 1_000, "dreamer": 160, "next": 40, "owed": 60 })
    );
}

#[test]
fn the_pace_text_for_a_chapter_says_what_is_left_in_it() {
    let chapter = Chapter {
        steps: 2.,
        shown: 1.,
        ..Chapter::fresh("s", 0.)
    };
    let text = pace_text(None, Some(&chapter), true);
    assert!(text.contains(
        "4 of 5 dreams left in this chapter; the next look in about 4 of Claude's steps"
    ));
    let spent = Chapter {
        shown: 5.,
        ..chapter
    };
    assert!(pace_text(None, Some(&spent), true).contains("No dreams left in this chapter"));
    assert_eq!(
        pace_text(None, None, false),
        "Dreams are off. Turn them on in /config: csr-companion, Dreams."
    );
    let looked = PaceRow {
        claude: 120_000.,
        dreamer: 12_000.,
        looks: 2.,
        dreams: 1.,
        ..PaceRow::fresh("s")
    };
    let said = pace_text(Some(&looked), None, true);
    let line = "This session: 2 looks, 1 dream, 12k tokens (10% of Claude's 120k).";
    assert!(said.contains(line));
}

#[test]
fn the_pace_registers_where_the_map_puts_it() {
    let mut t = Table::new();
    register(&mut t);
    assert_eq!(t.features("turn_complete"), ["pace"]);
    assert_eq!(t.features("step"), ["pace"]);
    assert_eq!(t.features("chapter"), ["pace"]);
    assert_eq!(t.features("start"), ["pace"]);
    assert!(
        t.entries("step")[0].event.is_none(),
        "a stream has no /event"
    );
    assert!(t.entries("chapter")[0].event.is_some() && t.entries("chapter")[0].after.is_some());
}

#[test]
fn a_keep_record_gets_its_repo_and_goes_to_the_dream_log() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::new(home.path().to_path_buf());
    let cwd = tempfile::tempdir().unwrap();
    let record = json!({ "kind": "chapter", "session": "s", "why": "start" });
    keep_record_in(&paths, cwd.path().to_str().unwrap(), record).unwrap();
    keep_record_in(&paths, "/nowhere", json!({ "kind": "answer", "id": "x" })).unwrap();
    let log = std::fs::read_to_string(paths.keep.join("dreams.jsonl")).unwrap();
    let rows: Vec<Value> = log
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["kind"], json!("chapter"));
    assert_eq!(
        rows[0]["repo"],
        Value::Null,
        "no git here and no csr project: no root"
    );
    assert!(rows[1].get("repo").is_none(), "an answer has no repo");
}
