//! The dreamer's tests: the removed TypeScript companion's, one for one by name, and the rest
//! of what the module does.
//!
//! The rig carries the real band (`band::register_with` and `register_late`, on a pretend
//! machine and pane). Where a TypeScript test looked for a note in the band, pressed one of its
//! buttons or let two prompts pass, the test here draws the band, presses the button by its
//! label and sends the prompt through the `digit` entries, the band's lifecycle among them. A
//! few tests still read the `view` row (`up`), where the TypeScript asked only whether a dream
//! was kept; they do not claim the band drew it.

use super::{
    answers::LoggedDream,
    outside::fake::{Fake, Gated, HOME, ROOT},
    text::*,
    *,
};
use crate::companion::{
    app::{
        band::{
            self,
            tests::kit::{self as bandkit, Drawn},
        },
        pace,
        pace::facts,
        state::strings,
    },
    relay::{
        dispatch::testing::{fixture_in, run, until, Fixture},
        host::fake::World,
    },
};
use std::{collections::HashSet, sync::Mutex, time::Duration};

const SID: &str = "sess-1";
/// What the dream of `dream_reply` says first, as the TypeScript's band tests find it.
const NOTES_READ: &str = "What if the notes also read";
/// Where the fake clock starts (the TypeScript's `NOW`).
const NOW: f64 = 1_791_547_200_000.;
/// What one step of Claude's costs: the four counts of `usage()`.
const LOOK: f64 = 41_000.;
const BRIEF: &str = "Things the person built in other sessions that came up in this work:\n* \
    skill notes-sync: built over 2 sessions, latest 08-26 [session dc24d328]";
const DREAM_LINE: &str = "What if the notes also read your side branches, so this week’s work \
    gets notes before it reaches main?";
const TITLE: &str = "Notes could follow side branches";
const NEWS: &str = "* plugin retry-queue: built over 1 session, latest 10-08 [session 3f2a91c0]";

fn dream_reply() -> String {
    [
        format!("dream: {DREAM_LINE}"),
        "explain:".to_string(),
        format!("**{TITLE}**"),
        "* CSR only checks main, which last moved on 09-21.".to_string(),
        "* On 08-26 you asked for CSR across projects [session dc24d328].".to_string(),
    ]
    .join("\n")
}

fn reply_of(line: &str, title: &str, body: &str) -> String {
    format!("dream: {line}\nexplain:\n**{title}**\n{body}")
}

/// What a fork costs where a test says nothing: the four counts.
fn used_usage() -> Value {
    json!({ "input_tokens": 900, "output_tokens": 100, "cache_creation_input_tokens": 0,
        "cache_read_input_tokens": 40_000 })
}

/// What a step of Claude's reports: the same counts, and the model.
fn usage() -> Value {
    let mut usage = used_usage();
    usage["model"] = json!("opus");
    usage
}

/// A look that costs 5 steps of Claude's.
fn big() -> Value {
    json!({ "input_tokens": 900, "output_tokens": 100, "cache_creation_input_tokens": 0,
        "cache_read_input_tokens": 5. * LOOK - 1_000. })
}

fn num(x: f64) -> Value {
    json!(x as i64)
}

/// A fake Claude Code whose `/config` says dreams are `dreams` (`None`: no row, so the default).
fn world(dreams: Option<bool>) -> World {
    let mut world = World::default();
    let rows = match dreams {
        Some(on) => json!([{ "key": "csr-companion.dreams", "label": "Dreams",
            "kind": "boolean", "value": on }]),
        None => json!([]),
    };
    let list = world.script.entry("config.list".into()).or_default();
    for _ in 0..3000 {
        list.push_back(Ok(rows.clone()));
    }
    world
}

fn replies(list: &[String]) -> World {
    World {
        replies: list.to_vec(),
        ..world(Some(true))
    }
}

type Kept = Arc<Mutex<Vec<(String, Value)>>>;

struct Rig {
    fx: Fixture,
    table: Table,
    out: Arc<Fake>,
    kept: Kept,
    next: Mutex<u32>,
}

fn rig_in(world: World, sid: &str) -> Rig {
    let kept: Kept = Arc::default();
    let seen = kept.clone();
    let keep: pace::Keep = Arc::new(move |cwd, record| {
        seen.lock().unwrap().push((cwd, record));
        Box::pin(async {})
    });
    let out = Arc::new(Fake::new(keep.clone(), BRIEF));
    let mut table = Table::new();
    band::register_with(&mut table, bandkit::gear().seams);
    pace::register_with(&mut table, keep.clone());
    register_with(&mut table, out.clone());
    band::register_late(&mut table);
    let fx = fixture_in(world, sid);
    pace::install_keep(&fx.cx, &keep);
    Rig {
        fx,
        table,
        out,
        kept,
        next: Mutex::new(0),
    }
}

fn rig(world: World) -> Rig {
    rig_in(world, SID)
}

/// Let the tasks the runtime holds run, and then some time pass: what the TypeScript tests'
/// `settle(() => false)` waited out.
async fn quiesce() {
    for _ in 0..30 {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_millis(15)).await;
}

impl Rig {
    fn cx(&self) -> Ctx {
        self.fx.cx.clone()
    }

    /// A context whose forks wait for the gate to open.
    fn gated(&self) -> (Ctx, Gated) {
        let gate = Gated::new(self.fx.host.clone());
        let cx = Ctx {
            host: Arc::new(gate.clone()),
            ..self.fx.cx.clone()
        };
        (cx, gate)
    }

    fn records(&self) -> Vec<Value> {
        let kept = self.kept.lock().unwrap();
        kept.iter().map(|(_, record)| record.clone()).collect()
    }

    fn find(&self, which: impl Fn(&Value) -> bool) -> Option<Value> {
        self.records().into_iter().find(|record| which(record))
    }

    /// The `--keep` records of `kind` whose `key` is `value`.
    fn with(&self, kind: &str, key: &str, value: &str) -> Vec<Value> {
        let records = self.records().into_iter();
        records
            .filter(|record| record["kind"] == kind && record[key] == value)
            .collect()
    }

    fn of_kind(&self, kind: &str) -> Vec<Value> {
        let records = self.records().into_iter();
        records.filter(|record| record["kind"] == kind).collect()
    }

    fn looks(&self) -> Vec<Value> {
        self.of_kind("look")
    }

    fn dreams(&self) -> Vec<Value> {
        self.of_kind("dream")
    }

    fn outcomes(&self, outcome: &str) -> usize {
        let looks = self.looks().into_iter();
        looks.filter(|look| look["outcome"] == outcome).count()
    }

    fn row(&self, sid: &str, key: &str) -> Value {
        let row = self.fx.cx.store.get(sid, key).unwrap();
        row.unwrap_or(Value::Null)
    }

    fn put(&self, sid: &str, key: &str, value: Value) {
        self.fx.cx.store.set(sid, key, &value).unwrap();
    }

    fn forks(&self) -> Vec<String> {
        self.fx.host.seen.lock().unwrap().forks.clone()
    }

    /// What the band draws now (`band` of the TypeScript's `mount`).
    async fn band(&self) -> Drawn {
        bandkit::band(&self.table, &self.fx.cx).await
    }

    /// Wait until the band shows text that matches `pattern`.
    async fn band_has(&self, pattern: &str) -> bool {
        for _ in 0..1000 {
            if self.band().await.has(pattern) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        false
    }

    /// Press the band's button labelled `label`; false when it draws none.
    async fn press(&self, label: &str) -> bool {
        bandkit::press(&self.table, &self.fx.cx, label).await
    }

    fn filled(&self) -> Vec<String> {
        self.fx.host.seen.lock().unwrap().filled.clone()
    }

    /// The line of the dream in the session's `view` row, if one is up.
    fn up(&self) -> Option<String> {
        let view = self.row(SID, "view");
        view["note"]["line"].as_str().map(str::to_string)
    }

    /// The `/after` of every entry of `key`: each entry gets what its own `/event` left.
    async fn after(&self, key: &str, cx: &Ctx, e: Value, packed: &Option<Value>) {
        for (at, entry) in self.table.entries(key).iter().enumerate() {
            let Some(after) = &entry.after else {
                continue;
            };
            let rows = packed.as_ref().and_then(Value::as_array);
            let left = rows
                .and_then(|rows| rows.iter().find(|row| row[0].as_u64() == Some(at as u64)))
                .map(|row| row[1].clone());
            let own = Ctx { left, ..cx.clone() };
            after(own, e.clone()).await.unwrap();
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
        self.after("step", cx, json!({ "e": e, "result": result }), &None)
            .await;
        // A look a step started runs while the next step comes, as the TypeScript's did.
        for _ in 0..3 {
            tokio::task::yield_now().await;
        }
    }

    /// `n` steps of a turn (the test file's `steps`): the main session's continue the turn's
    /// index where the last call stopped, `from` replays the indexes from there, a subagent's
    /// start at 0.
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

    async fn go(&self, n: u32) {
        let cx = self.cx();
        self.steps(&cx, n, None, None).await;
    }

    /// A compaction: the `/event` of the `chapter` gate, then its `/after`.
    async fn compact(&self, cx: &Ctx, trigger: &str) {
        let e = json!({ "trigger": trigger, "messages": [] });
        let before = run(&self.table, "chapter", cx, e).await;
        self.after("chapter", cx, json!({ "messages": [] }), &before.left)
            .await;
    }

    async fn start(&self, cx: &Ctx) {
        let e = json!({ "cwd": "/repo", "isInteractive": true, "startedAt": 0 });
        run(&self.table, "start", cx, e).await;
    }

    /// A prompt the person submitted (`$.prompt.submit`), through the dreamer's `digit` entry.
    async fn prompt(&self, cx: &Ctx, text: &str, kind: &str) {
        let e = json!({ "text": text, "origin": { "kind": kind } });
        run(&self.table, "digit", cx, e).await;
    }

    /// A chapter that started an hour ago, ten steps in, with one dream shown.
    fn hour_old_chapter(&self) {
        let chapter = json!({ "session": SID, "started": NOW - 3_600_000., "steps": 10,
            "due": 30, "looks": 1, "shown": 1, "adopted": 0 });
        self.put(SID, "chapter", chapter);
    }
}

// ---------------------------------------------------------------- the dream log, as a test's

const LOGGED_ID: &str = "dream:abc:1";

fn logged() -> String {
    [
        json!({ "kind": "dream", "id": LOGGED_ID, "at": NOW / 1000. - 60., "line": DREAM_LINE,
            "title": TITLE, "text": "x" }),
        json!({ "kind": "answer", "id": "other", "answer": "ignored", "at": NOW / 1000. - 30. }),
    ]
    .map(|record| record.to_string())
    .join("\n")
}

fn fill() -> String {
    ask_claude(DREAM_LINE, TITLE, "x")
}

const OVERDREAMT: &str = "To my knowledge the dreamer is overdreaming i cannot kill a session";

// ---------------------------------------------------------------- dreams on and off

#[tokio::test]
async fn dreams_are_on_by_default_with_no_option_set_a_look_can_run_as_claude_works() {
    let r = rig(World {
        replies: vec![dream_reply()],
        ..world(None)
    });
    r.go(30).await;
    assert!(until(|| !r.forks().is_empty()).await);
    assert!(!r.forks().is_empty());
    // `/csr dreams` says what the pace says: not that they are off.
    assert!(!pace::pace_text(None, None, true).contains("Dreams are off"));
}

#[tokio::test]
async fn dreams_turned_off_in_config_no_look_however_long_claude_works_and_csr_dreams() {
    let r = rig(World {
        replies: vec![dream_reply()],
        ..world(Some(false))
    });
    r.go(30).await;
    quiesce().await;
    assert_eq!(r.forks(), Vec::<String>::new());
    assert_eq!(r.looks(), Vec::<Value>::new());
    let said = pace::pace_text(None, None, false);
    assert!(said.contains("Dreams are off"));
    assert!(said.contains("/config"));
}

#[tokio::test]
async fn a_look_that_finds_nothing_does_not_slow_the_next_and_with_nothing_new_about_the() {
    let r = rig(replies(&["dream: none".into(), "dream: none".into()]));
    r.go(12).await;
    assert!(until(|| !r.forks().is_empty()).await);
    quiesce().await;
    assert_eq!(r.forks().len(), 1);
    let pace = r.row(SID, "pace");
    assert_eq!(
        (pace["looks"].clone(), pace["dreams"].clone()),
        (json!(1), json!(0))
    );
    assert!(
        !r.band().await.drew,
        "a look that found nothing leaves the band bare"
    );
    // Step 6 waits for its share; step 12 finds nothing.
    let looks = r.looks();
    assert_eq!(looks.len(), 2, "{looks:?}");
    assert_eq!(looks[0]["outcome"], "budget");
    assert_eq!(
        (looks[0]["every"].clone(), looks[0]["tokens"].clone()),
        (json!(6), json!(0))
    );
    assert_eq!(looks[1]["outcome"], "none");
    assert_eq!(looks[1]["every"], json!(22)); // (120 - 12) / 5
    assert_eq!(looks[1]["tokens"], num(LOOK));
    assert_eq!(looks[1]["left"], json!(5));
    assert_eq!(looks[1]["usage"], used_usage());
    assert_eq!(looks[1]["model"], "opus");
    assert!(looks[0].get("usage").is_none(), "no fork: nothing spent");

    r.go(30).await; // step 42, past the next look (34): affordable, but nothing new
    assert!(until(|| r.outcomes("quiet") >= 2).await); // 34 and 40
    assert_eq!(r.forks().len(), 1);
    let last = r.looks().pop().unwrap();
    assert_eq!(
        (last["outcome"].clone(), last["tokens"].clone()),
        (json!("quiet"), json!(0))
    );

    // A past line: not news.
    let past = "- 10-08 (csr-engine): “the import should retry” [session 3f2a91c0]";
    r.out.set_brief(&format!("{BRIEF}\n{past}"));
    r.go(12).await;
    assert!(until(|| r.outcomes("quiet") >= 4).await); // 46 and 52
    assert_eq!(r.forks().len(), 1);
    assert_eq!(r.looks().pop().unwrap()["outcome"], "quiet");

    r.out.set_brief(&format!("{BRIEF}\n{past}\n{NEWS}"));
    r.go(12).await;
    assert!(until(|| r.row(SID, "pace")["looks"].as_u64().unwrap_or(0) >= 2).await);
    assert_eq!(r.forks().len(), 2);
    assert_eq!(r.row(SID, "pace")["looks"], json!(2));
}

// ---------------------------------------------------------------- one dreamer per session

fn elsewhere() -> &'static str {
    "/elsewhere/dev-mods/csr-companion"
}

#[tokio::test]
async fn one_dreamer_per_session_a_fresh_lease_held_by_another_dev_copy_keeps_this_one() {
    let r = rig(replies(&[dream_reply()]));
    r.out.put_lease(
        SID,
        json!({ "root": elsewhere(), "rank": 1, "at": NOW - 60_000. }),
    );
    r.go(30).await;
    quiesce().await;
    assert_eq!(r.forks(), Vec::<String>::new());
    assert_eq!(
        r.looks(),
        Vec::<Value>::new(),
        "no budget, no quiet: it never looked"
    );
    assert_eq!(r.out.lease_file(SID)["root"], elsewhere());
}

#[tokio::test]
async fn one_dreamer_per_session_a_dev_copy_takes_the_lease_from_the_installed_plugin_and() {
    let installed = "/home/u/.claude/plugins/cache/claude-self-reflect/csr-companion/0.4.0";
    let r = rig(replies(&[dream_reply()]));
    r.out.put_lease(
        SID,
        json!({ "root": installed, "rank": 0, "at": NOW - 60_000. }),
    );
    r.go(12).await;
    assert!(until(|| !r.forks().is_empty()).await);
    quiesce().await;
    assert_eq!(r.forks().len(), 1);
    let lease = r.out.lease_file(SID);
    assert_ne!(lease["root"], installed);
    assert_eq!(lease["root"], ROOT);
    assert_eq!(lease["rank"], json!(1));
    assert_eq!(super::outside::rank_of(installed), 0);
}

/// Shape changed: `lease.py` and `python3` are gone. The lease step is the engine's own
/// (`lease::step_under_lock`), so the cases are the three it can end in: it ran and said held,
/// it ran and said not held, and it did not run (a crash, or its lock not got in time). Only the
/// first grants the lease. The TypeScript's last case, a `python3` that cannot start, had the
/// look go on with no lease kept; the engine is always there, so the one case left that keeps
/// no lease is no HOME (`no_home`): a take or a release is granted, a renewal is not.
#[tokio::test]
async fn one_dreamer_per_session_only_lease_py_saying_held_grants_the_lease_python3() {
    use super::outside::{lease_held, no_home};
    use crate::companion::lease::{LeaseOutput, Step};
    let said = |held: bool| {
        Ok(LeaseOutput {
            held,
            lease: json!({}),
        })
    };
    assert!(lease_held(&said(true)));
    assert!(!lease_held(&said(false)));
    // Its lock not got in time, crashed or killed: another copy decides now.
    assert!(!lease_held(&Err(anyhow::anyhow!(
        "timed out after 2000 ms"
    ))));
    assert!(!lease_held(&Err(anyhow::anyhow!("killed"))));
    // No HOME: nothing can be leased on this machine, and the look goes on.
    assert!(no_home(Step::Take) && no_home(Step::Release));
    assert!(
        !no_home(Step::Renew),
        "an active look cannot keep ownership with no lease"
    );
    // A lease step that did not run takes nothing: no look starts.
    let r = rig(replies(&[dream_reply()]));
    r.out
        .lease_fails
        .store(true, std::sync::atomic::Ordering::SeqCst);
    r.go(12).await;
    quiesce().await;
    assert_eq!(r.forks(), Vec::<String>::new());
    assert!(!r.out.steps("take").is_empty());
}

#[tokio::test]
async fn one_dreamer_per_session_a_lease_is_taken_under_a_lock_marked_busy_while_the_look() {
    let installed = "/home/u/.claude/plugins/cache/claude-self-reflect/csr-companion/0.5.0";
    let r = rig(replies(&[dream_reply()]));
    let (cx, gate) = r.gated();
    r.out.put_lease(
        SID,
        json!({ "root": installed, "rank": 0, "at": NOW - 60_000., "busy": true }),
    );
    r.steps(&cx, 12, None, None).await;
    quiesce().await;
    assert_eq!(
        gate.arrived(),
        0,
        "the installed copy's look runs: the dev copy waits"
    );
    r.out.put_lease(
        SID,
        json!({ "root": installed, "rank": 0, "at": NOW - 60_000. }),
    );
    r.steps(&cx, 12, None, None).await;
    assert!(until(|| gate.arrived() >= 1).await);
    let ours = r.out.lease_file(SID);
    assert_eq!(
        (ours["rank"].clone(), ours["busy"].clone()),
        (json!(1), json!(true))
    );
    let taken = r.out.steps("take");
    let last = taken.last().unwrap();
    let folder = format!("{HOME}/.claude-self-reflect/csr-companion/dreamer-lease");
    assert_eq!(last.folder.to_string_lossy(), folder);
    assert_eq!((last.session.as_str(), last.busy), (SID, true));
    gate.open();
    assert!(until(|| r.out.lease_file(SID).get("busy").is_none()).await);
    assert!(!r.out.steps("release").is_empty(), "the look ended");
}

#[tokio::test]
async fn one_dreamer_per_session_a_lease_older_than_15_minutes_is_taken_whoever_held_it() {
    let r = rig(replies(&[dream_reply()]));
    let stale = json!({ "root": elsewhere(), "rank": 1, "at": NOW - 16. * 60_000. });
    r.out.put_lease(SID, stale);
    r.go(12).await;
    assert!(until(|| !r.forks().is_empty()).await);
    assert_eq!(r.forks().len(), 1);
    assert_ne!(r.out.lease_file(SID)["root"], elsewhere());
}

// ---------------------------------------------------------------- a dream sent on

#[tokio::test]
async fn a_prompt_that_carries_a_dream_is_logged_sent_once_from_the_band_fill_or_the_pane() {
    let r = rig(world(Some(true)));
    r.out.set_log(&logged());
    r.put(SID, "looksToSkip", json!(1));
    r.put(SID, "typedPastInARow", json!(1));
    r.hour_old_chapter();
    let cx = r.cx();
    // The pane's send-text, with a band hotkey that landed in the box first.
    let text =
        format!("2CSR's dreamer asked this while you worked: “{DREAM_LINE}” Its reasoning: Notes.");
    r.prompt(&cx, &text, "composer").await;
    let sent = || r.with("answer", "answer", "sent");
    assert_eq!(sent().len(), 1, "{:?}", r.records());
    assert_eq!(sent()[0]["id"], LOGGED_ID);
    assert_eq!(sent()[0]["by"], "prompt");
    assert_eq!(
        r.row(SID, "looksToSkip"),
        json!(0),
        "an answer: the back-off stops"
    );
    assert_eq!(r.row(SID, "chapter")["adopted"], json!(1), "its place back");
    r.out.set_log(&format!("{}\n{}", logged(), sent()[0]));
    // The band's fill, sent again: already logged.
    r.prompt(&cx, &fill(), "composer").await;
    assert_eq!(sent().len(), 1);
    // A prompt that only talks about the dreamer is not a sending.
    r.prompt(&cx, "why did the dreamer ask that?", "composer")
        .await;
    assert_eq!(sent().len(), 1);
}

#[tokio::test]
async fn a_dream_sent_on_with_a_no_beside_it_is_rejected_with_your_words_never_sent_and() {
    let r = rig(world(Some(true)));
    r.out.set_log(&logged());
    r.put(SID, "looksToSkip", json!(2));
    r.hour_old_chapter();
    let cx = r.cx();
    let fill = ask_claude(DREAM_LINE, TITLE, "* CSR only checks main.");
    r.prompt(&cx, &format!("{fill}\n\n{OVERDREAMT}"), "composer")
        .await;
    let answers = r.of_kind("answer");
    assert_eq!(answers.len(), 1, "{answers:?}");
    assert_eq!(answers[0]["id"], LOGGED_ID);
    assert_eq!(answers[0]["answer"], "rejected");
    assert_eq!(answers[0]["by"], "prompt");
    assert_eq!(answers[0]["said"], OVERDREAMT);
    assert_eq!(
        r.row(SID, "looksToSkip"),
        json!(0),
        "an answer: the back-off stops"
    );
    assert_eq!(r.row(SID, "chapter")["adopted"], json!(0));
}

#[tokio::test]
async fn your_next_prompt_after_a_dream_you_sent_rejects_it_when_it_says_no_only_the_next() {
    let r = rig(world(Some(true)));
    r.out.set_log(&logged());
    r.hour_old_chapter();
    let cx = r.cx();
    let said = |answer: &str| r.with("answer", "answer", answer);
    r.prompt(&cx, &fill(), "composer").await;
    assert_eq!(said("sent").len(), 1);
    assert_eq!(r.row(SID, "chapter")["adopted"], json!(1));
    r.out.set_log(&format!("{}\n{}", logged(), said("sent")[0]));
    let no = "No, that won't work: we can't restart a session from the phone.";
    r.prompt(&cx, no, "composer").await;
    let rejected = said("rejected");
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0]["id"], LOGGED_ID);
    assert_eq!(rejected[0]["by"], "reply");
    assert_eq!(rejected[0]["said"], no);
    assert_eq!(
        r.row(SID, "chapter")["adopted"],
        json!(0),
        "its place, given back at sent, is taken again"
    );
    // A second reply is not a reply to it.
    r.prompt(&cx, "no, really not", "composer").await;
    assert_eq!(said("rejected").len(), 1);
}

#[tokio::test]
async fn a_reply_that_agrees_or_one_after_the_half_hour_leaves_the_dream_sent() {
    let r = rig(world(Some(true)));
    r.out.set_log(&logged());
    let cx = r.cx();
    let said = |answer: &str| r.with("answer", "answer", answer);
    let agree = format!("{}\n\nyes, try it on one branch first", fill());
    r.prompt(&cx, &agree, "composer").await;
    assert_eq!(said("sent").len(), 1);
    r.prompt(&cx, "go on, I can't wait to see it", "composer")
        .await;
    assert_eq!(said("rejected"), Vec::<Value>::new());
    // Sent afresh, then a no 31 minutes on: no longer a reply to it.
    r.out.set_log(&logged());
    r.prompt(&cx, &fill(), "bridge").await;
    r.fx.clock.set(NOW + 31. * 60_000.);
    r.prompt(&cx, "no, not that", "composer").await;
    assert_eq!(said("rejected"), Vec::<Value>::new());
}

fn logged_dream(line: &str, at: f64) -> LoggedDream {
    LoggedDream {
        id: String::new(),
        line: line.to_string(),
        at,
    }
}

#[test]
fn the_words_beside_a_dream_are_all_the_person_s_and_the_sent_dream_is_the_one_its() {
    let a = ("What if the import reused the upload queue?", "t", "x");
    let b = ("What if we disable validation?", "t", "x");
    let dreams = [logged_dream(a.0, 1.), logged_dream(b.0, 2.)];
    let reply = "CSR's dreamer asked this while you worked yesterday, but this is not useful.";
    let said = words_beside(&format!("{}\n\n{reply}", ask_claude(a.0, a.1, a.2)));
    assert_eq!(said, reply);
    assert!(rebuts(&said));
    let quoting = format!(
        "{}\n\nNever follow “What if we disable validation?”",
        ask_claude(a.0, a.1, a.2)
    );
    let found = |text: &str| dream_sent(text, &dreams).map(|d| d.line.clone());
    assert_eq!(found(&quoting), Some(a.0.to_string()));
    let pane = "CSR's dreamer asked this while you worked, quoted: “What if the import reused \
        the upload queue?” Its reasoning: “x.” Never “What if we disable validation?”";
    assert_eq!(found(pane), Some(a.0.to_string()));
    let other = "CSR's dreamer asked this while you worked yesterday: “What if we disable \
        validation?”";
    assert_eq!(
        found(other),
        None,
        "a header and a line that is not the dream's"
    );
    let says = [
        OVERDREAMT,
        "nope",
        "I can’t do that from the phone",
        "this is not what I asked",
        "yes please",
        "no-op is fine",
        "not now, after the tests",
    ]
    .map(rebuts);
    assert_eq!(says, [true, true, true, true, false, false, false]);
}

#[test]
fn dreamsent_finds_the_dream_a_prompt_carries_and_nothing_in_prose_that_names_the() {
    let dreams = [
        logged_dream("What if a?", 1.),
        logged_dream("What if b?", 2.),
    ];
    let at = |text: &str, from: &[LoggedDream]| dream_sent(text, from).map(|d| d.at);
    let carried =
        "What if b?\n\nCSR's dreamer asked this while you worked. Its reasoning:\n\n> **t**";
    assert_eq!(at(carried, &dreams), Some(2.));
    let header = "CSR's dreamer asked this while you worked: “What if a?” Its reasoning: t.";
    assert_eq!(at(header, &dreams), Some(1.));
    assert_eq!(
        at("What if b? I wonder", &dreams),
        None,
        "no header: the person's own words"
    );
    let prose = "I read \"CSR's dreamer asked this while you worked\" in the log";
    assert_eq!(at(prose, &dreams), None);
    // v2 writes a question's own quotes straight; the dream is still found, from the pane and
    // the band.
    let line = "What if the pane said “fresh” first?";
    let fresh = [logged_dream(line, 3.)];
    let pane = "CSR's dreamer asked this while you worked, quoted: “What if the pane said \
        \"fresh\" first?” Its reasoning: “t.”";
    assert_eq!(at(pane, &fresh), Some(3.));
    assert_eq!(at(&ask_claude(line, "t", "x"), &fresh), Some(3.));
    assert_eq!(
        at(&format!("2{}", ask_claude("What if b?", "t", "x")), &dreams),
        Some(2.)
    );
    let nope = format!("{}\n\nnope, overdreaming", ask_claude(line, "t", "x"));
    assert_eq!(words_beside(&nope), "nope, overdreaming");
    assert_eq!(words_beside(&format!("{pane} no thanks")), "no thanks");
}

// ---------------------------------------------------------------- what a look keeps

#[tokio::test]
async fn a_look_keeps_how_long_it_took_and_why_a_fork_came_back_empty_a_chapter_says() {
    let r = rig(World {
        fork_reason: Some("api-error".into()),
        ..world(Some(true))
    });
    let cx = r.cx();
    r.start(&cx).await;
    r.go(12).await;
    assert!(until(|| r.outcomes("none") > 0).await);
    let none = r.find(|record| record["outcome"] == "none").unwrap();
    assert_eq!(none["reason"], "api-error");
    assert_eq!(none["took_ms"], json!(0));
    let budget = r.find(|record| record["outcome"] == "budget").unwrap();
    assert_eq!(budget["took_ms"], json!(0));
    assert!(budget.get("reason").is_none());
    let chapter = r.find(|record| record["kind"] == "chapter").unwrap();
    assert_eq!(chapter["interactive"], json!(true));
}

// C6: the 10-11 twins, and the closest unrelated pair of the 18 dreams.
type Said = (&'static str, &'static str);
const HEARTS_1: Said = (
    "What if your hearts also ranked the feed, so the kinds of items you like rise first, as \
     likes do on Instagram?",
    "Hearts could steer the feed order",
);
const HEARTS_2: Said = (
    "What if the hearts you give in the pane told the dreamer which kinds of questions land, so \
     it aims its next ones there?",
    "Your hearts could steer the dreamer",
);
const INKS: Said = (
    "What if the pane took its colours from pigment's nyt inks, the palette your 10.x deck \
     already uses, instead of hand-picked shades?",
    "The pane could share the deck's palette",
);
const BROCHURE: Said = (
    "What if the dreams-off pane showed the dream you marked helpful on 10-10 as its sample, \
     instead of describing dreams?",
    "The brochure could show your own dream",
);

fn entry(said: Said) -> String {
    seen_entry(said.0, Some(said.1))
}

fn near(said: Said, against: &str) -> bool {
    near_dream(said.0, Some(said.1), against)
}

#[test]
fn a_near_twin_of_a_dream_already_offered_is_a_repeat_the_closest_unrelated_pair_is() {
    assert!(jaccard(&entry(HEARTS_1), &entry(HEARTS_2)) >= NEAR);
    assert!(near(HEARTS_2, &entry(HEARTS_1)));
    assert!(!near(BROCHURE, &entry(INKS)));
    // The exact match still holds, old entries too.
    assert!(near_dream(
        &HEARTS_1.0.to_uppercase(),
        Some("x"),
        HEARTS_1.0
    ));
}

#[tokio::test]
async fn a_twin_of_a_seen_dream_is_logged_as_a_repeat_and_never_shown() {
    let twin = reply_of(
        HEARTS_2.0,
        HEARTS_2.1,
        "The hearts you give could teach the dreamer.",
    );
    let r = rig(replies(&[twin]));
    r.put(MACHINE, "seenDreams", json!([entry(HEARTS_1)]));
    r.go(12).await;
    assert!(until(|| r.outcomes("repeat") > 0).await);
    let paid = r
        .find(|record| record["kind"] == "look" && record["outcome"] == "repeat")
        .unwrap();
    assert_eq!(paid["tokens"], num(LOOK));
    assert_eq!(r.up(), None);
    assert_eq!(r.row(MACHINE, "seenDreams"), json!([entry(HEARTS_1)]));
}

// Dreams in the person's language.
const HI_HEARTS_1: Said = (
    "क्या हो अगर आपके दिल फ़ीड को भी क्रम दें, ताकि जिस प्रकार की चीज़ें आपको पसंद हैं वे पहले \
     आएँ, जैसे इंस्टाग्राम पर लाइक?",
    "दिल फ़ीड के क्रम को दिशा दे सकते हैं",
);
const HI_HEARTS_2: Said = (
    "क्या हो अगर पेन में आपके दिए दिल ड्रीमर को बताएँ कि किस प्रकार के सवाल काम आते हैं, \
     ताकि वह अगले सवाल वहीं पूछे?",
    "आपके दिल ड्रीमर को दिशा दे सकते हैं",
);
const HI_INKS: Said = (
    "क्या हो अगर पेन अपने रंग पिगमेंट की nyt स्याही से ले, वही पैलेट जो आपका 10.x डेक पहले से \
     इस्तेमाल करता है, हाथ से चुने रंगों के बजाय?",
    "पेन डेक का पैलेट साझा कर सकता है",
);
const HI_BROCHURE: Said = (
    "क्या हो अगर ड्रीम्स-बंद पेन वह सपना दिखाए जिसे आपने 10-10 को मददगार बताया था, सपनों का \
     वर्णन करने के बजाय?",
    "ब्रोशर आपका अपना सपना दिखा सकता है",
);
const JA_NOTES_1: Said = (
    "もしノートがサイドブランチも読んだら、今週の作業がメインに届く前にノートが付くのでは？",
    "ノートはサイドブランチも読める",
);
const JA_NOTES_2: Said = (
    "もしノートがサイドブランチも読めば、メインに入る前に今週の作業にノートが付くのでは？",
    "ノートがサイドブランチを追える",
);
const JA_INKS: Said = (
    "もしペインの色をデッキのパレットから取ったら、手で選んだ色の代わりになるのでは？",
    "ペインはデッキの色を使える",
);
const MIXED_1: Said = (
    "What if हम CSR के recap को हर session की शुरुआत में default बना दें?",
    "Recap हर session में",
);
const MIXED_2: Said = (
    "What if pane के hearts dreamer को बताएँ कि कौन से सवाल काम आते हैं?",
    "Hearts से dreamer सीखे",
);
const AR: &str = "ماذا لو قرأت الملاحظات الفروع الجانبية أيضًا؟";

fn asked(line: &str) -> Option<String> {
    parse_dream(&format!("dream: {line}\nexplain:\n**A title**\nWhy.")).map(|dream| dream.line)
}

#[test]
fn a_dream_in_any_language_is_a_question_when_it_ends_on_its_question_mark_no() {
    for line in [
        DREAM_LINE,
        "Have you thought about one queue for both jobs?",
        HI_HEARTS_1.0,
        JA_NOTES_1.0,
        AR,
        MIXED_1.0,
    ] {
        assert_eq!(asked(line).as_deref(), Some(line));
    }
    // Bold and quotes off.
    assert_eq!(
        asked(&format!("**“{}”**", HI_BROCHURE.0)).as_deref(),
        Some(HI_BROCHURE.0)
    );
    assert!(
        asked(&format!("{}?", "क".repeat(239))).is_some(),
        "240 characters"
    );
    assert!(asked(&format!("{}?", "क".repeat(240))).is_none(), "241");
    assert!(
        asked(&format!("{}？", "𠮷".repeat(199))).is_some(),
        "200 characters, 399 units"
    );
    for line in [
        "none",
        "none in Hindi either",
        "What if we tested it first.",
        "क्या हो अगर हम इसे पहले आज़माएँ",
        "ノートがサイドブランチも読む。",
    ] {
        assert!(asked(line).is_none(), "{line}");
    }
    // ASCII is no sign of English: the English openings are accepted, never required.
    for line in [
        "Et si on partageait le cache entre les services ?",
        "Was wäre, wenn wir den Cache teilen?",
        "Y si compartimos la cache entre los servicios?",
        "Wenn wir den Cache teilen, was dann?",
        "Should we test it first?",
        "« Et si on partageait le cache ? »",
    ] {
        assert!(asked(line).is_some(), "{line}");
    }
    assert_eq!(
        asked("« Et si on partageait le cache ? »").as_deref(),
        Some("Et si on partageait le cache ?")
    );
    assert!(parse_dream(HI_HEARTS_1.0).is_none(), "no dream: label");
}

#[test]
fn spaced_scripts_are_told_apart_and_their_twins_caught_at_the_english_near() {
    assert!(
        !same_dream(HI_INKS.0, HI_BROCHURE.0),
        "keyed '' both, they were one dream"
    );
    assert!(!same_dream("?", "؟"));
    for (a, b) in [
        (HI_INKS, HI_BROCHURE),
        (HI_HEARTS_1, HI_INKS),
        (HI_HEARTS_2, HI_BROCHURE),
        (JA_NOTES_1, JA_INKS),
        (MIXED_1, MIXED_2),
        (MIXED_1, HI_HEARTS_1),
    ] {
        assert!(!near(b, &entry(a)), "{a:?} {b:?}");
    }
    assert!(jaccard(&entry(HI_HEARTS_1), &entry(HI_HEARTS_2)) >= NEAR);
    assert!(near(HI_HEARTS_2, &entry(HI_HEARTS_1)));
    for dream in [HI_HEARTS_1, JA_NOTES_1, MIXED_1] {
        assert!(
            near_dream(dream.0, Some("x"), &entry(dream)),
            "a true duplicate"
        );
    }
    // Character pairs of Japanese share its grammar: unspaced scripts match exactly (NFKC,
    // punctuation aside) until a filter is calibrated for them.
    let cache: Said = (
        "もしキャッシュを共有したらどうでしょうか？",
        "キャッシュ共有",
    );
    let screen: Said = ("もし画面を暗くしたらどうでしょうか？", "画面の配色");
    assert!(!near(screen, &entry(cache)));
    let ascii_mark = "もしキャッシュを共有したらどうでしょうか?";
    assert!(
        near_dream(ascii_mark, Some("別の題"), &entry(cache)),
        "？ or ?"
    );
    assert!(!near(JA_NOTES_2, &entry(JA_NOTES_1)));
    assert!(near_dream(
        &format!("{} ", JA_INKS.0),
        Some(JA_INKS.1),
        &entry(JA_INKS)
    ));
    assert!(near(HEARTS_2, &entry(HEARTS_1)), "English as before");
    assert!(!near(BROCHURE, &entry(INKS)));
}

#[test]
fn fuzzy_twins_only_where_near_is_calibrated_elsewhere_an_exact_match_punctuation() {
    let fr_cache: Said = (
        "Et si on partageait le cache entre les services ?",
        "Un cache commun",
    );
    let fr_status: Said = (
        "Et si on affichait le statut dans le panneau ?",
        "Un statut visible",
    );
    let parsed = |said: Said| {
        parse_dream(&format!(
            "dream: {}\nexplain:\n**{}**\nWhy.",
            said.0, said.1
        ))
        .map(|dream| dream.line)
    };
    for said in [fr_cache, fr_status] {
        assert!(parsed(said).is_some(), "{said:?}");
    }
    assert!(!same_dream(fr_cache.0, fr_status.0));
    assert!(!near(fr_status, &entry(fr_cache)));
    let again = "Et si on partageait le cache entre les services?";
    assert!(near_dream(again, Some("Autre"), &entry(fr_cache)));
    let ja_a: Said = (
        "もしキャッシュを共有したらどうでしょうか？",
        "キャッシュ共有",
    );
    let ja_b: Said = (
        "もしキャッシュを共有したら、どうでしょうか？",
        "キャッシュ共有",
    );
    for said in [ja_a, ja_b] {
        assert!(parsed(said).is_some(), "{said:?}");
    }
    assert!(same_dream(ja_a.0, ja_b.0));
    assert!(near(ja_b, &entry(ja_a)));
    // A space, an ideographic space, 。
    assert!(same_dream(
        "もしキャッシュを 共有したら\u{3000}どうでしょうか。",
        ja_a.0
    ));
    assert!(!same_dream("もし画面を暗くしたらどうでしょうか？", ja_a.0));
    assert!(
        near(HEARTS_2, &entry(HEARTS_1)),
        "English twins, as calibrated"
    );
    assert!(near(HI_HEARTS_2, &entry(HI_HEARTS_1)), "Hindi twins too");
}

#[tokio::test]
async fn a_hindi_dream_that_repeats_a_seen_one_is_dropped_a_distinct_one_is_shown() {
    let reply = |said: Said| reply_of(said.0, said.1, "* [session dc24d328]");
    let r = rig(replies(&[reply(HI_HEARTS_2), reply(HI_BROCHURE)]));
    r.put(
        MACHINE,
        "seenDreams",
        json!([entry(HI_HEARTS_1), entry(HI_INKS)]),
    );
    r.go(12).await;
    assert!(until(|| r.outcomes("repeat") > 0).await);
    assert_eq!(r.up(), None);
    // News: the next look is taken.
    r.out.set_brief(&format!("{BRIEF}\n{NEWS}"));
    r.go(40).await;
    assert!(until(|| r.up().is_some_and(|line| line.contains("ड्रीम्स-बंद पेन"))).await);
    let dream = r.dreams().pop().unwrap();
    assert_eq!(dream["line"], HI_BROCHURE.0);
}

// C4: receipts are checked and marked, never a gate.
#[test]
fn receipts_this_session_reads_this_session_an_unknown_session_or_commit_is_marked() {
    let brief = "* skill notes-sync: built over 2 sessions [session dc24d328]";
    let text = "On 08-26 [session dc24d328]. Here [session abcd1234]. Elsewhere [session \
        0450a37e]. Fixed in [commit f46448c], [commit 1234567].";
    let missing: HashSet<String> = ["1234567".to_string()].into();
    assert_eq!(
        mark_receipts(text, brief, "abcd1234-0f0f", &missing),
        "On 08-26 [session dc24d328]. Here [this session]. Elsewhere [session 0450a37e] (not in \
         CSR's notes: unchecked). Fixed in [commit f46448c], [commit 1234567] (not found in this \
         repo: unchecked)."
    );
    let prompt = dream_prompt(brief, &[], &[], &[], "");
    assert!(prompt.contains("never cite this session"));
    assert!(prompt.contains("marked as the person's own words"));
}

#[tokio::test]
async fn a_dream_citing_this_session_and_a_commit_the_repo_lacks_is_shown_its_receipts() {
    let body = "* We did this here [session abcd1234].\n* Landed in [commit abcdef1].";
    let r = rig_in(
        replies(&[reply_of(DREAM_LINE, TITLE, body)]),
        "abcd1234-0f0f",
    );
    r.out.commits.lock().unwrap().insert("f46448c".into());
    let cx = r.cx();
    r.steps(&cx, 12, None, None).await;
    assert!(until(|| !r.dreams().is_empty()).await);
    let text = r.dreams()[0]["text"].as_str().unwrap().to_string();
    assert!(text.contains("We did this here [this session]."), "{text}");
    assert!(
        text.contains("[commit abcdef1] (not found in this repo: unchecked)"),
        "{text}"
    );
}

// C5: the person's last three prompts.
#[tokio::test]
async fn the_person_s_last_three_prompts_reach_the_dreamer_as_already_on_the_table() {
    let r = rig(world(Some(true)));
    let cx = r.cx();
    let pasted = "the system should offer to enable dreams and be a brochure \
        <pasted_content id=\"a\">pane</pasted_content>";
    let forwarded = "Here is a note offered by a side agent:\n\n> You should know · x";
    for text in ["first ask", pasted, forwarded, "third ask", "fourth ask"] {
        r.prompt(&cx, text, "composer").await;
    }
    assert_eq!(
        r.row(SID, "asked"),
        json!([
            "the system should offer to enable dreams and be a brochure",
            "third ask",
            "fourth ask"
        ])
    );
    r.go(12).await;
    assert!(until(|| !r.forks().is_empty()).await);
    let prompt = &r.forks()[0];
    assert!(prompt.contains("already on the table"));
    assert!(prompt.contains("- the system should offer to enable dreams and be a brochure"));
    assert!(!prompt.contains("first ask"));
}

#[tokio::test]
async fn a_forwarded_card_story_or_dream_is_never_what_the_person_asked_for_a_reply_they() {
    let card = "CSR's compaction card (compacted by csr): Context: 274k → 70k tokens. Learnt: \
        Never skip validation.";
    let forward = ask_claude(
        "What if the import reused the upload queue?",
        "Reuse the queue",
        "* It backs off already.",
    );
    assert_eq!(own_prompt(card), None);
    assert_eq!(
        own_prompt(&format!("{card}\nwhy did it keep that?")).as_deref(),
        Some("why did it keep that?")
    );
    assert_eq!(
        own_prompt("CSR's story of this session, rev 2: We moved the queue."),
        None
    );
    let side = "Here is a note offered by a side agent:\n\n> Heads up · x\n>\n> **T**\n\nwhy?";
    assert_eq!(own_prompt(side).as_deref(), Some("why?"));
    let csr = "Here is a note offered by CSR (claude-self-reflect):\n\n> Heads up · y";
    assert_eq!(own_prompt(csr), None);
    let beside = format!("{forward}\n\nNo, keep the current palette.\n\n{forward}");
    assert_eq!(
        own_prompt(&beside).as_deref(),
        Some("No, keep the current palette.")
    );
    let sidebar = "CSR's compaction card (the one in the sidebar) should show tokens";
    assert_eq!(own_prompt(sidebar).as_deref(), Some(sidebar));
    let mine = "CSR's dreamer asked this while you worked yesterday, but I wrote this one";
    assert_eq!(own_prompt(mine).as_deref(), Some(mine));
    let r = rig(world(Some(true)));
    let cx = r.cx();
    for text in [card.to_string(), format!("{card}\n\nkeep the card")] {
        r.prompt(&cx, &text, "composer").await;
    }
    assert_eq!(r.row(SID, "asked"), json!(["keep the card"]));
    let kept = strings(&cx, SID, "asked");
    let types = Dream {
        line: "What if we checked types instead of validation?".into(),
        title: "t".into(),
        text: String::new(),
    };
    assert_eq!(offers_another_way(&types, &kept), None);
}

// 10-12 19:51: "instead of compacting", 22 and 29 minutes after the person settled on compacting.
const DECIDED: [&str; 2] = [
    "understood, one thing i manually do repeatedly is once context goes 180-250 mark based on \
     where we are i compact to avoid exchanges",
    "done here means i will never fire compact just notice that is going to happen, on \
     unattended sessions and with optin it should just trigger",
];
const RESTART_LINE: &str = "What if an unattended session at the top of your band restarted \
    fresh with CSR's recap, instead of compacting?";
const RESTART_TITLE: &str = "Restarting could beat compacting";
const RESTART_TEXT: &str = "* CSR could start a fresh session with its recap, rather than run a \
    compaction.\n* On 06-10 you wrote \"my vision: you kill a session, you restart\" [session \
    0bab445f].";

fn restart() -> Dream {
    Dream {
        line: RESTART_LINE.into(),
        title: RESTART_TITLE.into(),
        text: RESTART_TEXT.into(),
    }
}

fn plain(line: &str, text: &str) -> Dream {
    Dream {
        line: line.into(),
        title: "t".into(),
        text: text.into(),
    }
}

#[tokio::test]
async fn a_dream_that_offers_another_way_to_what_your_last_three_prompts_decided_is() {
    let reply = reply_of(RESTART_LINE, RESTART_TITLE, RESTART_TEXT);
    let r = rig(replies(&[reply]));
    r.put(SID, "asked", json!(DECIDED));
    r.go(12).await;
    assert!(until(|| r.outcomes("decided") > 0).await);
    let decided = r.find(|record| record["outcome"] == "decided").unwrap();
    assert_eq!(decided["against"], DECIDED[0]);
    assert_eq!(r.up(), None);
    assert!(r.forks()[0].contains("or another way to them"));
    let against = |dream: &Dream, asked: &[&str]| {
        let asked: Vec<String> = asked.iter().map(|line| line.to_string()).collect();
        offers_another_way(dream, &asked)
    };
    assert_eq!(against(&restart(), &DECIDED).as_deref(), Some(DECIDED[0]));
    assert_eq!(
        against(&restart(), &["fix the importer"]),
        None,
        "nothing it replaces was decided"
    );
    let queue = plain(
        "What if the import reused the upload queue, instead of a second one?",
        "",
    );
    assert_eq!(against(&queue, &DECIDED), None);
    let compacts = plain(
        "What if the import reused the upload queue?",
        "It compacts less.",
    );
    assert_eq!(against(&compacts, &DECIDED), None, "no other way offered");
}

// 10-12 19:51: the same dream's whole reason was a line typed 06-10.
const OLD_LINES: &str = "Lines the person typed in past sessions, in their own words, that sit \
    close to this work (a line older than 30 days is context only: never the main reason to \
    ask):\n- 06-10 (claude-self-reflect): “my vision: you kill a session, you restart” [session \
    0bab445f] (124 days old: context only)\n- 10-08 (csr-engine): “the import should retry” \
    [session 3f2a91c0]";

#[tokio::test]
async fn a_dream_whose_reason_is_only_a_line_older_than_30_days_is_dropped_one_that() {
    let reply = reply_of(RESTART_LINE, RESTART_TITLE, RESTART_TEXT);
    let r = rig(replies(&[reply]));
    r.out.set_brief(&format!("{BRIEF}\n\n{OLD_LINES}"));
    r.go(12).await;
    assert!(until(|| r.outcomes("stale") > 0).await);
    assert_eq!(r.up(), None);
    assert!(r.forks()[0].contains("context only"));
    let old = |dream: &Dream, brief: &str, session: &str| leans_on_old(dream, brief, session);
    assert!(old(&restart(), OLD_LINES, "sess-1"));
    let quoted = Dream {
        text: "You wrote \"you kill a session, you restart\" once.".into(),
        ..restart()
    };
    assert!(old(&quoted, OLD_LINES, "sess-1"), "its words, no receipt");
    let more = |extra: &str| Dream {
        text: format!("{RESTART_TEXT}\n{extra}"),
        ..restart()
    };
    let commit = more("* The recap landed in [commit 19e7d46].");
    assert!(!old(&commit, OLD_LINES, "sess-1"));
    let newer = more("* On 10-08 you asked for retries [session 3f2a91c0].");
    assert!(!old(&newer, OLD_LINES, "sess-1"));
    let here = more("* Here [session abcd1234].");
    assert!(
        old(&here, OLD_LINES, "abcd1234-0f0f"),
        "this session is no newer reason"
    );
    assert!(
        !old(&restart(), BRIEF, "sess-1"),
        "no old line in the brief"
    );
}

// ---------------------------------------------------------------- the chapter

#[tokio::test]
async fn each_chapter_s_first_look_is_taken_even_when_csr_s_notes_hold_nothing_new_later() {
    let none = "dream: none".to_string();
    let r = rig(replies(&[none.clone(), none.clone(), none]));
    r.put(SID, "briefFacts", json!(facts(BRIEF))); // the dreamer has seen every note already
    r.go(12).await;
    assert!(until(|| !r.forks().is_empty()).await);
    assert_eq!(r.forks().len(), 1, "the chapter's fresh look");
    r.go(30).await; // the next look is due at 34: nothing new, so no fork
    assert!(until(|| r.outcomes("quiet") >= 1).await);
    assert_eq!(r.forks().len(), 1);
    let cx = r.cx();
    r.compact(&cx, "auto").await;
    r.go(12).await;
    assert!(until(|| r.forks().len() >= 2).await);
    assert_eq!(r.forks().len(), 2, "a new chapter: a fresh look again");
}

#[tokio::test]
async fn a_look_still_running_when_a_compaction_starts_the_next_chapter_leaves_that() {
    let r = rig(replies(&[dream_reply()]));
    let (cx, gate) = r.gated();
    r.steps(&cx, 12, None, None).await;
    assert!(until(|| gate.arrived() >= 1).await);
    r.compact(&cx, "auto").await;
    assert!(
        until(|| r
            .find(|c| c["kind"] == "chapter" && c["why"] == "compact")
            .is_some())
        .await
    );
    let fresh = r.row(SID, "chapter");
    gate.open();
    assert!(until(|| r.outcomes("stale") > 0).await);
    assert_eq!(
        r.row(SID, "chapter"),
        fresh,
        "due, looks and shown as the compaction set them"
    );
    assert!(!r.band().await.drew, "the stale look shows nothing");
    let stale = r.find(|record| record["outcome"] == "stale").unwrap();
    assert!(stale["tokens"].is_number());
}

#[tokio::test]
async fn a_compaction_carries_the_dream_that_is_up_once_marked_still_open_the_next_one() {
    let r = rig(replies(&[dream_reply()]));
    let cx = r.cx();
    r.go(12).await;
    assert!(r.band_has(NOTES_READ).await);
    r.compact(&cx, "manual").await;
    let view = r.row(SID, "view");
    assert_eq!(view["carried"], json!(true));
    assert!(r.band_has("still open from before the compaction").await);
    assert!(r.band().await.has(NOTES_READ));
    let carried = r.with("answer", "answer", "carried");
    assert_eq!(carried.len(), 1, "{:?}", r.records());
    let chapter = r.row(SID, "chapter");
    assert_eq!(
        (chapter["shown"].clone(), chapter["steps"].clone()),
        (json!(0), json!(0))
    );
    // Two prompts again to answer it.
    assert_eq!(view["promptsSurvived"], json!(0));
    r.prompt(&cx, "one", "human").await;
    assert!(r.band().await.has(NOTES_READ));
    r.compact(&cx, "manual").await;
    assert!(!r.band().await.has(NOTES_READ));
    let answers: Vec<Value> = r
        .of_kind("answer")
        .into_iter()
        .map(|record| record["answer"].clone())
        .collect();
    assert_eq!(answers, [json!("carried"), json!("expired")]);
}

#[tokio::test]
async fn a_dream_typed_past_is_never_asked_again_and_the_dreamer_backs_off() {
    let r = rig(replies(&[dream_reply(), dream_reply(), dream_reply()]));
    let cx = r.cx();
    r.go(12).await;
    assert!(r.band_has(NOTES_READ).await);
    r.prompt(&cx, "one", "human").await;
    assert!(
        r.band().await.has(NOTES_READ),
        "two prompts again to answer it"
    );
    r.prompt(&cx, "two", "human").await;
    assert!(!r.band().await.has(NOTES_READ));
    assert_eq!(r.row(SID, "looksToSkip"), json!(1));
    let answers = r.row(MACHINE, "answers");
    assert_eq!(
        answers.as_array().unwrap().last().unwrap()["answer"],
        "ignored"
    );

    r.out.set_brief(&format!("{BRIEF}\n{NEWS}"));
    r.go(27).await; // step 39, when the next look is due: skipped, backing off
    quiesce().await;
    assert_eq!(r.forks().len(), 1);
    let last = r.looks().pop().unwrap();
    assert_eq!(last["outcome"], "skip");

    r.go(20).await; // step 59: looks again, but the same dream is not shown twice
    assert!(until(|| r.forks().len() >= 2).await);
    quiesce().await;
    assert_eq!(r.forks().len(), 2);
    assert!(
        r.forks()[1].contains(DREAM_LINE),
        "the seen list rides in the prompt"
    );
    assert!(!r.band().await.has(NOTES_READ));
}

#[tokio::test]
async fn a_dream_about_the_pane_that_cites_no_other_session_still_reaches_you_you_judge() {
    // The one dream acted on (10-09, asked Claude): about the pane, no receipt.
    let pigment = [
        "dream: What if the pane took its colours from pigment's nyt inks, the palette your \
         10.x deck already uses?",
        "explain:",
        "**The pane could share the deck's palette**",
        "* We're tuning the pane's colours by hand.",
    ]
    .join("\n");
    let r = rig(replies(&[pigment]));
    r.go(12).await;
    assert!(r.band_has("pigment's nyt inks").await);
    assert_eq!(r.dreams().len(), 1);
    assert!(r.find(|record| record["outcome"] == "ungrounded").is_none());
}

#[tokio::test]
async fn a_dream_answered_in_the_pane_leaves_the_band_and_is_never_counted_as_typed_past() {
    let r = rig(replies(&[dream_reply()]));
    let cx = r.cx();
    r.go(12).await;
    assert!(r.band_has(NOTES_READ).await);
    let id = r.dreams()[0]["id"].as_str().unwrap().to_string();
    let said = json!({ "kind": "dream", "id": id, "line": DREAM_LINE });
    let answer = json!({ "kind": "answer", "id": id, "answer": "helpful", "at": 1, "by": "pane" });
    r.out.set_log(&format!("{said}\n{answer}\n"));
    r.prompt(&cx, "one", "human").await;
    assert!(!r.band().await.has(NOTES_READ));
    assert_eq!(r.of_kind("answer").len(), 0, "the pane logged it, once");
    assert_eq!(r.row(SID, "looksToSkip").as_i64().unwrap_or(0), 0);
    let answers = r.row(MACHINE, "answers");
    let rows = answers.as_array().unwrap();
    assert!(rows
        .iter()
        .any(|row| row["id"] == id && row["answer"] == "helpful"));
    assert!(rows.iter().all(|row| row["answer"] != "ignored"));
}

#[tokio::test]
async fn a_dream_about_csr_s_own_dream_cycle_still_reaches_you_only_the_pane_and_the() {
    let work = dream_reply().replace(
        "What if the notes also read",
        "What if the dream cycle also read",
    );
    let r = rig(replies(&[work]));
    r.go(12).await;
    assert!(r.band_has("What if the dream cycle also read").await);
    assert_eq!(r.dreams().len(), 1);
}

#[tokio::test]
async fn the_dim_4_on_a_dream_turns_dreams_off_after_a_second_press() {
    let r = rig(replies(&[dream_reply()]));
    r.go(12).await;
    assert!(r.band_has(NOTES_READ).await);

    assert!(r.press("Disable").await);
    assert!(r.fx.host.seen.lock().unwrap().configured.is_empty());
    assert!(r.band().await.has_button("Press again to turn dreams off"));

    assert!(r.press("Press again to turn dreams off").await);
    let configured = r.fx.host.seen.lock().unwrap().configured.clone();
    assert_eq!(
        configured,
        [json!({ "key": "csr-companion.dreams", "value": false })]
    );
    let last = r.records().pop().unwrap();
    assert_eq!(
        (last["kind"].clone(), last["answer"].clone()),
        (json!("answer"), json!("turn_off"))
    );
    assert!(!r.band().await.has(NOTES_READ));
}

#[tokio::test]
async fn a_dream_waits_while_you_should_know_asks_so_its_4_can_never_turn_the_other_band() {
    let r = rig(replies(&[dream_reply()]));
    let cx = r.cx();
    r.go(12).await;
    assert!(until(|| !r.forks().is_empty()).await);
    assert!(
        r.band_has(NOTES_READ).await,
        "the dream is up when nothing asks"
    );
    // You should know's band, below this one, asks with its own 4.
    let asking = json!({ "type": "Box", "props": {}, "children": [
        { "type": "Button", "props": { "key": "ysk-disable", "hotkey": "4", "plain": true,
            "label": "Disable" }, "children": [] }] });
    let under = bandkit::draw(&r.table, &cx, asking).await;
    assert!(under.is_none(), "the band leaves the one below as it is");

    // Prompts typed while the dream was hidden are not "typed past".
    r.prompt(&cx, "one", "human").await;
    r.prompt(&cx, "two", "human").await;
    assert!(r
        .records()
        .iter()
        .all(|record| record["answer"] != "ignored"));
    assert_eq!(r.row(SID, "looksToSkip"), Value::Null);

    // As You should know's band redraws when its note clears: the dream, with its own 4.
    let shown = r.band().await;
    assert!(shown.has(NOTES_READ));
    assert_eq!(shown.key_of("Disable").as_deref(), Some("band-disable"));
    assert!(shown.key_of("ysk-disable").is_none());
}

#[tokio::test]
async fn a_dream_that_cannot_be_turned_off_says_so_and_stays_up() {
    let world = World {
        config_refused: true,
        ..replies(&[dream_reply()])
    };
    let r = rig(world);
    r.go(12).await;
    assert!(r.band_has(NOTES_READ).await);
    assert!(r.press("Disable").await);
    assert!(r.press("Press again to turn dreams off").await);
    let shown = r.band().await;
    assert!(shown.has("Couldn't turn dreams off here. Use /config."));
    assert!(shown.has(NOTES_READ));
    assert!(
        shown.has_button("Disable"),
        "the confirm is asked for again"
    );
}

#[tokio::test]
async fn already_thought_of_it_keeps_the_dream_from_being_asked_again() {
    let r = rig(replies(&[dream_reply()]));
    r.go(12).await;
    assert!(r.band_has(NOTES_READ).await);
    assert!(r.press("Already thought of it").await);
    assert!(!r.band().await.has(NOTES_READ));
    let seen = strings(&r.cx(), MACHINE, "knownDreams");
    assert_eq!(seen, [seen_entry(DREAM_LINE, Some(TITLE))]);
    let last = r.records().pop().unwrap();
    assert_eq!(last["answer"], "thought_of_it");
}

#[tokio::test]
async fn learn_more_explains_the_dream_and_ask_claude_hands_it_over() {
    let r = rig(replies(&[dream_reply()]));
    r.go(12).await;
    assert!(r.band_has(NOTES_READ).await);
    assert!(r.press("Learn more").await);
    let explained = r.band().await;
    assert!(explained.has(TITLE));
    assert!(explained.has("this look 41,000 tokens"));
    assert!(r.press("Ask Claude").await);
    let filled = r.filled();
    assert_eq!(filled.len(), 1);
    assert!(filled[0].starts_with(&format!("“{DREAM_LINE}”")));
    assert!(!r.band().await.has(NOTES_READ));
    let last = r.records().pop().unwrap();
    assert_eq!(last["answer"], "ask_claude");
}

// ---------------------------------------------------------------- You should know's fork

const YSK_BRIEF: &str = "Files Claude handled lately, newest first: hooks/register.tsx (12×).\n\
    \n\
    Code that changed after one of the person's past sessions, or this one before its last \
    compaction, worked on it:\n\
    * handle in src/setup.rs changed [commit fdaba23, session 3f2a91c0]\n\
    \n\
    Rules the person set for these files:\n\
    * never block Claude Code from a hook [session 1e0e1cf4]\n\
    \n\
    Lines the person typed in past sessions, in their own words, that sit close to this work:\n\
    - 07-11 (demo-app): “what if there was a free station” [session 142ef262]\n\
    \n\
    Dreams already offered here, and what the person answered:\n\
    - 10-12: “What if the recap…” [helpful]";
const YSK_PROMPT: &str =
    "## Overview\n\nlearn: none\n\n## Conclusion\n\nSay learn: none when in doubt.";

#[test]
fn yskfacts_keeps_the_brief_s_code_rule_correction_and_past_session_lines_and_drops() {
    let facts = ysk_facts(YSK_BRIEF);
    assert!(facts.contains("[commit fdaba23, session 3f2a91c0]"));
    assert!(facts.contains("never block Claude Code from a hook"));
    assert!(facts.contains("Lines the person typed in past sessions"));
    assert!(
        facts.contains("- 07-11 (demo-app): “what if there was a free station” [session 142ef262]")
    );
    assert!(!facts.contains("Files Claude handled"));
    assert!(!facts.contains("What if the recap"));
    assert_eq!(ysk_facts("Files Claude handled lately: a.ts (1×)."), "");
}

#[test]
fn withcsrfacts_goes_before_you_should_know_s_conclusion_and_leaves_a_prompt_alone() {
    let out = with_csr_facts(YSK_PROMPT, &ysk_facts(YSK_BRIEF));
    let head = out.find("## What CSR remembers near this session").unwrap();
    assert!(head < out.find("## Conclusion").unwrap());
    assert!(out.starts_with("## Overview"));
    assert!(out.ends_with("Say learn: none when in doubt."));
    assert_eq!(with_csr_facts(YSK_PROMPT, "  "), YSK_PROMPT);
    assert!(is_ysk_propose(YSK_PROMPT));
    assert!(!is_ysk_propose(
        "Answer straight away: do not think it over first."
    ));
}

#[test]
fn isyskorigin_takes_every_name_the_you_should_know_seat_answers_to_and_no_other() {
    for plugin in [
        "cc-plugin-you-should-know",
        "cc-plugin-you-should-know@builtin",
        "you-should-know",
        "suggest-learning@builtin",
    ] {
        assert!(is_ysk_origin(plugin), "{plugin}");
    }
    for plugin in [
        "csr-shifts",
        "csr-companion@claude-self-reflect",
        "engine",
        "cc-plugin-tips@builtin",
    ] {
        assert!(!is_ysk_origin(plugin), "{plugin}");
    }
}

#[test]
fn yskline_reads_a_reply_as_you_should_know_does_and_citedfrom_counts_only_receipts() {
    assert_eq!(ysk_line("learn: none"), (None, true));
    let said = "- learn: The setup handler changed after your 07-12 session.\ntag: Heads up";
    assert_eq!(
        ysk_line(said),
        (
            Some("The setup handler changed after your 07-12 session.".to_string()),
            true
        )
    );
    assert_eq!(ysk_line("I could not decide."), (None, false));
    let facts = ysk_facts(YSK_BRIEF);
    let reply = "It changed in commit fdaba23 [session 3f2a91c0], and commit abcdef1 too.";
    assert_eq!(
        cited_from(reply, &facts),
        ["commit fdaba23", "session 3f2a91c0"]
    );
}

#[test]
fn the_dreamer_s_fork_opens_with_the_side_request_frame_no_tools_one_response_no() {
    let prompt = dream_prompt("* a fact [session 3f2a91c0]", &[], &[], &[], "");
    assert!(prompt.starts_with("<system-reminder>This is a side request from CSR's dreamer"));
    assert!(prompt.contains("You have no tools"));
    assert!(prompt.find("</system-reminder>").unwrap() < prompt.find("## Overview").unwrap());
}

#[test]
fn a_hand_off_is_text_paste_placeholders_and_ultra_words_cannot_act_and_the_sent() {
    assert_eq!(
        clean_for_draft("see @hooks/register.tsx and ultrathink"),
        "see \u{FF20}hooks/register.tsx and ultra-think"
    );
    assert_eq!(clean_for_draft("mail me at a@b.co"), "mail me at a@b.co");
    let line = "What if @pane.py drew the brief?";
    let sent = ask_claude(line, "The pane could draw it", "x");
    let logged = [logged_dream(line, 1.)];
    let found = dream_sent(&sent, &logged);
    assert_eq!(found.map(|dream| dream.line.clone()).as_deref(), Some(line));
}

// ---------------------------------------------------------------- the rest of what it does

fn fork_event(plugin: &str, prompt: &str) -> Value {
    json!({ "prompt": prompt, "origin": { "plugin": plugin, "tier": "builtin" } })
}

/// The fork gate as the app runs it: `/event`, then `/after` with `result`.
async fn forked(r: &Rig, e: Value, result: Value) -> Reply {
    let cx = r.cx();
    let before = run(&r.table, "fork", &cx, e).await;
    r.after("fork", &cx, result, &before.left).await;
    before
}

#[tokio::test]
async fn a_fork_of_another_plugin_is_noted_and_passes_untouched() {
    let r = rig(world(Some(true)));
    let before = forked(
        &r,
        fork_event("csr-shifts", YSK_PROMPT),
        json!({ "value": {} }),
    )
    .await;
    assert!(before.next.is_none() && before.answer.is_none());
    assert!(until(|| !r.records().is_empty()).await);
    let record = r.records().remove(0);
    assert_eq!(record["kind"], "ysk");
    assert_eq!(record["outcome"], "other-origin");
    assert_eq!(record["origin"], "csr-shifts/builtin");
    assert_eq!(record["session"], SID);
    assert_eq!(r.out.briefs.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn you_should_know_s_propose_fork_gets_csr_s_facts_and_a_ysk_record_of_what_came() {
    let r = rig(world(Some(true)));
    r.out.set_brief(YSK_BRIEF);
    let reply = json!({ "isAnswered": true,
        "text": "learn: The setup handler changed [commit fdaba23].", "usage": used_usage() });
    let before = forked(
        &r,
        fork_event("cc-plugin-you-should-know", YSK_PROMPT),
        json!({ "value": reply }),
    )
    .await;
    let sent = before.next.expect("the prompt carries CSR's facts");
    let prompt = sent["prompt"].as_str().unwrap();
    assert!(prompt.contains("## What CSR remembers near this session"));
    assert!(prompt.contains("[commit fdaba23, session 3f2a91c0]"));
    assert!(until(|| !r.records().is_empty()).await);
    let record = r.records().remove(0);
    assert_eq!(record["kind"], "ysk");
    assert_eq!(record["prompt"], "propose");
    assert_eq!(record["outcome"], "line");
    assert_eq!(
        record["line"],
        "The setup handler changed [commit fdaba23]."
    );
    assert_eq!(record["cited"], json!(["commit fdaba23"]));
    assert!(record["given"].as_u64().unwrap() > 0);
    assert_eq!(record["usage"], used_usage());
    // One brief per session per five minutes.
    let again = forked(
        &r,
        fork_event("you-should-know", YSK_PROMPT),
        json!({ "value": reply }),
    )
    .await;
    assert!(again.next.is_some());
    assert_eq!(r.out.briefs.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_fork_of_you_should_know_that_is_denied_or_empty_or_not_a_propose_is_kept_as_such() {
    let r = rig(world(Some(true)));
    r.out.set_brief(YSK_BRIEF);
    let ysk = "cc-plugin-you-should-know";
    forked(
        &r,
        fork_event(ysk, YSK_PROMPT),
        json!({ "deny": "timed out" }),
    )
    .await;
    let empty = json!({ "value": { "isAnswered": false, "reason": "api-error" } });
    forked(&r, fork_event(ysk, YSK_PROMPT), empty).await;
    let explain = fork_event(ysk, "Explain it straight away.");
    let text = json!({ "isAnswered": true, "text": "Because." });
    let unfacted = forked(&r, explain, json!({ "value": text })).await;
    assert!(unfacted.next.is_none(), "an explain prompt is left alone");
    r.out
        .no_ysk_brief
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let off = forked(&r, fork_event(ysk, YSK_PROMPT), json!({ "value": text })).await;
    assert!(
        off.next.is_none(),
        "CSR_NO_YSK_BRIEF keeps the record and leaves the prompt alone"
    );
    assert!(until(|| r.records().len() >= 4).await);
    let outcomes: Vec<Value> = r
        .records()
        .iter()
        .map(|record| record["outcome"].clone())
        .collect();
    assert_eq!(
        outcomes,
        [
            json!("denied"),
            json!("api-error"),
            json!("explained"),
            json!("unparsed")
        ]
    );
    assert_eq!(r.records()[2]["prompt"], "explain");
}

/// `path` posted as the relay posts it; the calls the engine asks of Claude Code are answered
/// with nothing, until the reply is final.
async fn posted(
    app: &Arc<crate::companion::relay::dispatch::App>,
    path: &str,
    body: Value,
) -> Value {
    let mut body = body;
    body["v"] = json!(env!("CARGO_PKG_VERSION"));
    let mut reply = app.handle(path, &body).await;
    while reply.get("calls").is_some() {
        let results: Vec<Value> = reply["calls"]
            .as_array()
            .unwrap()
            .iter()
            .map(|call| json!({ "id": call["id"], "v": null }))
            .collect();
        let step = json!({ "v": env!("CARGO_PKG_VERSION"), "id": reply["id"], "results": results });
        reply = app.handle("/step", &step).await;
    }
    reply
}

#[tokio::test]
async fn a_fork_whose_seat_the_relay_posts_beside_the_event_is_told_apart_by_it() {
    use crate::companion::{
        relay::{
            dispatch::{App, Log},
            host::{Hub, SystemClock},
        },
        store::Store,
    };
    let dir = tempfile::tempdir().unwrap();
    let kept: Kept = Arc::default();
    let seen = kept.clone();
    let keep: pace::Keep = Arc::new(move |cwd, record| {
        seen.lock().unwrap().push((cwd, record));
        Box::pin(async {})
    });
    let out = Arc::new(Fake::new(keep.clone(), BRIEF));
    out.set_brief(YSK_BRIEF);
    let mut table = Table::new();
    pace::register_with(&mut table, keep);
    register_with(&mut table, out);
    let (push, _lines) = tokio::sync::mpsc::unbounded_channel();
    let app = App::new(
        Hub::new(push),
        Arc::new(Store::at(dir.path().join("csr-companion/companion.db"))),
        Arc::new(SystemClock),
        Arc::new(Log::default()),
        table,
    );
    let at = |k: &str, e: Value| json!({ "sid": SID, "cwd": "/repo", "k": k, "e": e });
    posted(
        &app,
        "/event",
        at("start", json!({ "isInteractive": true })),
    )
    .await;
    let fork = |plugin: Option<&str>| {
        let mut body = at("fork", json!({ "prompt": YSK_PROMPT }));
        if let Some(plugin) = plugin {
            body["origin"] = json!({ "plugin": plugin, "tier": "builtin" });
        }
        body
    };
    // You should know's own fork: CSR's facts go in, and the seat does not go on with the event.
    let ours = posted(&app, "/event", fork(Some("cc-plugin-you-should-know"))).await;
    let prompt = ours["next"]["prompt"].as_str().expect("the facts went in");
    assert!(
        prompt.contains("## What CSR remembers near this session"),
        "{ours}"
    );
    assert!(ours["next"].get("origin").is_none(), "{ours}");
    assert!(ours["id"].is_string());
    // Another plugin's fork, and one with no seat: passed untouched, and logged as such.
    for (plugin, shown) in [
        (Some("csr-shifts"), "csr-shifts/builtin"),
        (None, "unknown"),
    ] {
        let theirs = posted(&app, "/event", fork(plugin)).await;
        assert_eq!(theirs, json!({}), "{shown}");
        let found = || {
            let kept = kept.lock().unwrap();
            kept.iter()
                .map(|(_, record)| record.clone())
                .any(|record| record["outcome"] == "other-origin" && record["origin"] == shown)
        };
        assert!(until(found).await, "{shown}: {:?}", kept.lock().unwrap());
    }
}

#[tokio::test]
async fn a_headless_child_never_forks_a_look_or_keeps_a_fork_record() {
    let r = rig(World {
        headless: true,
        replies: vec![dream_reply()],
        ..world(Some(true))
    });
    r.fx.cx.session.lock().unwrap().interactive = Some(false);
    r.go(30).await;
    let ysk = "cc-plugin-you-should-know";
    forked(&r, fork_event(ysk, YSK_PROMPT), json!({ "value": {} })).await;
    let cx = r.cx();
    r.compact(&cx, "auto").await;
    quiesce().await;
    assert_eq!(r.forks(), Vec::<String>::new());
    assert_eq!(r.records(), Vec::<Value>::new());
}

#[tokio::test]
async fn a_compaction_with_no_dream_up_or_one_a_subagent_ran_carries_nothing() {
    let r = rig(replies(&[dream_reply()]));
    let cx = r.cx();
    r.go(12).await;
    assert!(until(|| r.up().is_some()).await);
    for e in [
        json!({ "trigger": "auto", "agentId": "sub-1", "messages": [] }),
        json!({ "trigger": "precompute", "messages": [] }),
    ] {
        let before = run(&r.table, "chapter", &cx, e).await;
        r.after("chapter", &cx, json!({ "messages": [] }), &before.left)
            .await;
    }
    assert_eq!(r.row(SID, "view")["carried"], Value::Null);
    // A compaction that did not go through carries nothing either.
    let before = run(&r.table, "chapter", &cx, json!({ "trigger": "auto" })).await;
    r.after("chapter", &cx, json!({ "skip": true }), &before.left)
        .await;
    assert_eq!(r.row(SID, "view")["carried"], Value::Null);
    assert!(r.with("answer", "answer", "carried").is_empty());
}

#[tokio::test]
async fn a_compaction_that_failed_leaves_the_dream_that_is_up_as_it_was() {
    let r = rig(replies(&[dream_reply()]));
    let cx = r.cx();
    r.go(12).await;
    assert!(until(|| r.up().is_some()).await);
    let view = r.row(SID, "view");
    let chapter = r.row(SID, "chapter");
    for _ in 0..2 {
        // The relay tells the engine the call threw: `/after` with `{ deny }`.
        let before = run(&r.table, "chapter", &cx, json!({ "trigger": "manual" })).await;
        r.after(
            "chapter",
            &cx,
            json!({ "deny": "summary failed" }),
            &before.left,
        )
        .await;
    }
    assert_eq!(r.row(SID, "view"), view, "not carried, not expired");
    assert_eq!(r.row(SID, "chapter"), chapter, "no new chapter");
    assert!(r.with("answer", "answer", "carried").is_empty());
    assert!(r.with("answer", "answer", "expired").is_empty());
}

#[tokio::test]
async fn the_dreamer_registers_where_the_map_puts_it() {
    let mut t = Table::new();
    register(&mut t);
    for key in ["step", "fork", "chapter", "digit", "start"] {
        assert_eq!(t.features(key), ["dreamer"], "{key}");
    }
    assert!(
        t.entries("step")[0].event.is_none(),
        "a stream has no /event"
    );
    let chapter = &t.entries("chapter")[0];
    assert!(chapter.event.is_some() && chapter.after.is_some());
}

#[tokio::test]
async fn a_start_hands_the_fleet_its_look_and_sets_the_pane_answer_job_every_15_seconds() {
    let r = rig(world(Some(true)));
    let cx = r.cx();
    assert!(cx.kept(|f: &mut Fleet| f.dream.is_none()));
    r.start(&cx).await;
    assert!(cx.kept(|f: &mut Fleet| f.dream.is_some()));
    // A dream is up, and the pane answers it: the 15 s job takes it off the band.
    let note = json!({ "id": "dream:x:1", "kind": "dream", "line": DREAM_LINE, "at": 1 });
    let view = json!({ "kind": "offer", "note": note, "tag": "Dream", "shownAt": 1,
        "promptsSurvived": 0 });
    r.put(SID, "view", view.clone());
    let said = json!({ "kind": "answer", "id": "dream:x:1", "answer": "helpful", "by": "pane" });
    r.out.set_log(&said.to_string());
    r.fx.timers
        .advance(15_000., Arc::new(r.fx.host.clone()))
        .await;
    assert!(until(|| r.up().is_none()).await);
    // A start that is not a person's sets no job.
    let kid = rig(world(Some(true)));
    let kid_cx = kid.cx();
    let e = json!({ "cwd": "/repo", "isInteractive": false });
    run(&kid.table, "start", &kid_cx, e).await;
    kid.put(SID, "view", view);
    kid.out.set_log(&said.to_string());
    kid.fx
        .timers
        .advance(15_000., Arc::new(kid.fx.host.clone()))
        .await;
    quiesce().await;
    assert!(kid.up().is_some());
}

#[tokio::test]
async fn the_spend_of_a_look_that_lost_the_lease_is_kept_and_its_dream_is_not_shown() {
    let r = rig(World {
        fork_usage: vec![big()],
        replies: vec![dream_reply()],
        ..world(Some(true))
    });
    let (cx, gate) = r.gated();
    r.steps(&cx, 12, None, None).await;
    assert!(until(|| gate.arrived() >= 1).await);
    // Another copy holds the lease now.
    r.out
        .put_lease(SID, json!({ "root": elsewhere(), "rank": 9, "at": NOW }));
    gate.open();
    let paid = || r.row(SID, "pace")["dreamer"].as_f64().unwrap_or(0.) > 0.;
    assert!(until(paid).await);
    let pace = r.row(SID, "pace");
    assert_eq!(pace["dreamer"], num(5. * LOOK));
    assert_eq!(pace["looks"], json!(1));
    quiesce().await;
    assert_eq!(
        r.dreams(),
        Vec::<Value>::new(),
        "the look was dropped; the money was not forgotten"
    );
    assert_eq!(r.up(), None);
}

#[test]
fn the_lease_args_name_the_pane_folder_the_session_and_the_rank_of_this_copy() {
    use super::outside::{lease_args, rank_of};
    use crate::companion::{lease::Step, paths::Paths};
    let paths = Paths::new(HOME.into());
    let args = lease_args(&paths, ROOT, "sess-1", Step::Take, true, 5.);
    assert_eq!(
        args.folder.to_string_lossy(),
        "/home/u/.claude-self-reflect/csr-companion/dreamer-lease"
    );
    assert_eq!(
        (args.session.as_str(), args.root.as_str(), args.rank),
        ("sess-1", ROOT, 1)
    );
    assert!(args.busy);
    assert_eq!(
        rank_of("/home/u/.claude/plugins/cache/x/csr-companion/0.4.0"),
        0
    );
    assert_eq!(rank_of("/home/u/dev-mods/csr-companion"), 1);
}

#[test]
fn optional_observations_are_independent_bounded_and_never_pad() {
    use super::text::parse_sections;
    let legacy = "dream: What if the upload queue could serve imports?\nexplain:\n**Reuse the queue**\nTry it.";
    let reply = format!(
        r#"{legacy}
confusing_claude: [{{"line":"Repeated the rejected approach.","evidence":"Turn 12: 'keep it local'"}},{{"line":"Misread the target.","evidence":"Turn 15: 'the pane'"}},{{"line":"Third.","evidence":"Turn 18"}}]
should_be: [{{"kind":"hook","line":"Check formatting before each commit.","evidence":"Turns 4 and 12: 'run fmt'"}},{{"kind":"other","line":"Invalid kind.","evidence":"Turn 9"}}]"#
    );
    let sections = parse_sections(&reply);
    assert_eq!(sections["confusing_claude"].as_array().unwrap().len(), 2);
    assert_eq!(sections["should_be"].as_array().unwrap().len(), 1);
    assert_eq!(parse_dream(&reply), parse_dream(legacy));
    for bad in [
        "",
        "confusing_claude: nope",
        "confusing_claude: []\nshould_be: []",
        "confusing_claude: [{\"line\":\"No citation\"}]",
    ] {
        let sections = parse_sections(bad);
        assert!(sections["confusing_claude"].as_array().unwrap().is_empty());
        assert!(sections["should_be"].as_array().unwrap().is_empty());
        assert_eq!(
            parse_dream(&format!("{legacy}\n{bad}")),
            parse_dream(legacy)
        );
    }
    let none = parse_sections("dream: none\nconfusing_claude: []\nshould_be: []");
    assert_eq!(none["should_be"], serde_json::json!([]));
}

#[tokio::test]
async fn same_fork_persists_optional_sections_even_with_no_what_if() {
    for first in [dream_reply(), "dream: none".into()] {
        let reply = format!(
            r#"{first}
confusing_claude: [{{"line":"Misread the target.","evidence":"Turn 4: 'the pane'"}}]
should_be: [{{"kind":"memory","line":"Keep the pane question first.","evidence":"Turns 4 and 8: 'question first'"}}]"#
        );
        let r = rig(replies(&[reply]));
        r.go(12).await;
        assert!(until(|| r.records().iter().any(|r| r["confusing_claude"].is_array())).await);
        let record = r
            .records()
            .into_iter()
            .find(|r| r["confusing_claude"].is_array())
            .unwrap();
        assert_eq!(record["confusing_claude"].as_array().unwrap().len(), 1);
        assert_eq!(record["should_be"][0]["kind"], "memory");
        assert_eq!(r.forks().len(), 1);
    }
}
