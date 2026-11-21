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

// ---------------------------------------------------------------- one dreamer per session

fn elsewhere() -> &'static str {
    "/elsewhere/dev-mods/csr-companion"
}

fn logged_dream(line: &str, at: f64) -> LoggedDream {
    LoggedDream {
        id: String::new(),
        line: line.to_string(),
        at,
    }
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

// 10-12 19:51: the same dream's whole reason was a line typed 06-10.
const OLD_LINES: &str = "Lines the person typed in past sessions, in their own words, that sit \
    close to this work (a line older than 30 days is context only: never the main reason to \
    ask):\n- 06-10 (claude-self-reflect): “my vision: you kill a session, you restart” [session \
    0bab445f] (124 days old: context only)\n- 10-08 (csr-engine): “the import should retry” \
    [session 3f2a91c0]";

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

#[path = "tests/dream_tests.rs"]
mod dream_tests;

#[path = "tests/dream_fork_tests.rs"]
mod dream_fork_tests;
