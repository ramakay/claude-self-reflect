//! The pane_cmd tests: the TypeScript ones of `companion.test.ts` that are about `/csr`, the pane,
//! its refresh and the pane opened, on a fake Claude Code and a fake `Outside`.

use super::*;
use crate::companion::relay::{
    dispatch::testing::{fixture_in, run, Fixture},
    host::fake::World,
};
use regex::Regex;
use std::sync::Mutex;

const ENGINE: &str = "/home/u/.local/bin/csr-engine";

fn rows() -> Value {
    json!([
        { "file": "src/setup.rs", "symbol": "handle", "verb": "changed", "oid": "fdaba2308c80",
            "date": "2026-09-21", "subject": "fix(installer)" },
        { "file": "src/lib.rs", "symbol": null, "verb": "changed", "oid": "fdaba2308c80",
            "date": "2026-09-21", "subject": "fix(installer)" },
    ])
}

fn shifts() -> Value {
    json!({
        "repo": "/repo", "total": 2,
        "status": "☾ CSR · 0 dreams/7d · 1 changed since · reminded agent 3×/7d · 0 tok",
        "reminded_7d": 3, "shifts": rows(), "changes_total": 1,
        "changes": [{ "conversation": "c1", "title": "remove the dream statusline",
            "current": false, "talked": "2026-09-20", "commits": 1, "shifts": rows() }],
        "dreams": [], "dreams_7d": 0, "dream_tokens_7d": 0,
    })
}

/// A first session in a folder: nothing found yet, but CSR has been at work this week.
fn quiet() -> Value {
    let mut quiet = shifts();
    for (key, value) in [
        ("total", json!(0)),
        ("shifts", json!([])),
        ("changes", json!([])),
        ("changes_total", json!(0)),
        ("recalled", json!(2)),
        ("corrections_carried_7d", json!(3)),
        ("shifts_told_7d", json!(1)),
        ("git", json!(true)),
    ] {
        quiet[key] = value;
    }
    quiet
}

fn no_repo() -> Value {
    json!({ "repo": null, "total": 0, "shifts": [], "changes": [], "changes_total": 0,
        "dreams": [], "status": null })
}

/// What `Outside` is asked, and what it answers.
struct Fake {
    herdr: bool,
    /// The shifts data; none: the read timed out.
    data: Mutex<Option<Value>>,
    asked: Mutex<Vec<(String, String)>>,
    runs: Mutex<Vec<Vec<String>>>,
}

impl Fake {
    fn new(herdr: bool, data: Option<Value>) -> Arc<Self> {
        Arc::new(Self {
            herdr,
            data: Mutex::new(data),
            asked: Mutex::default(),
            runs: Mutex::default(),
        })
    }

    fn runs(&self) -> Vec<Vec<String>> {
        self.runs.lock().unwrap().clone()
    }

    fn ran(&self, sub: &str) -> Option<Vec<String>> {
        self.runs()
            .into_iter()
            .find(|argv| argv.get(2).map(String::as_str) == Some(sub))
    }
}

fn ran(stdout: Value) -> Option<Ran> {
    Some(Ran {
        code: 0,
        stdout: stdout.to_string(),
    })
}

#[async_trait]
impl Outside for Fake {
    async fn shifts(&self, cwd: &str, sid: &str) -> Option<Value> {
        self.asked.lock().unwrap().push((cwd.into(), sid.into()));
        self.data.lock().unwrap().clone()
    }

    fn env(&self, name: &str) -> Option<String> {
        match name {
            "HERDR_PANE_ID" if self.herdr => Some("w2:p1".into()),
            "HERDR_BIN_PATH" if self.herdr => Some("/bin/herdr".into()),
            _ => None,
        }
    }

    async fn run(&self, argv: Vec<String>) -> Option<Ran> {
        self.runs.lock().unwrap().push(argv.clone());
        match argv.get(2).map(String::as_str) {
            Some("get") => ran(json!({ "result": { "pane": {
                "pane_id": argv[3], "tab_id": "w2:t1", "workspace_id": "w2" } } })),
            Some("list") => {
                // Only Claude's own space.
                if argv.get(3).map(String::as_str) != Some("--workspace")
                    || argv.get(4).map(String::as_str) != Some("w2")
                {
                    return ran(json!({ "result": { "panes": [] } }));
                }
                ran(json!({ "result": { "panes": [
                    { "pane_id": "w2:p1", "tab_id": "w2:t1" },
                    { "pane_id": "w2:p2", "tab_id": "w2:t1", "label": "Shifts" },
                ] } }))
            }
            Some("split") => ran(json!({ "result": { "pane": { "pane_id": "w2:p9" } } })),
            _ => ran(json!({})),
        }
    }

    fn engine(&self) -> Option<String> {
        Some(ENGINE.into())
    }
}

struct Rig {
    fx: Fixture,
    table: Table,
    outside: Arc<Fake>,
}

/// A fake Claude Code whose `/config` row for dreams is `on`, for the next 60 reads.
fn world(dreams: bool, refused: bool) -> World {
    let mut world = World {
        config_refused: refused,
        ..World::default()
    };
    let list = world.script.entry("config.list".into()).or_default();
    for _ in 0..60 {
        list.push_back(Ok(
            json!([{ "key": "csr-companion.dreams", "label": "Dreams",
            "kind": "boolean", "value": dreams }]),
        ));
    }
    world
}

fn rig_with(world: World, outside: Arc<Fake>) -> Rig {
    let mut table = Table::new();
    register_with(&mut table, outside.clone());
    register_late_with(&mut table, outside.clone());
    Rig {
        fx: fixture_in(world, "sess-1"),
        table,
        outside,
    }
}

fn rig(dreams: bool, herdr: bool, data: Value) -> Rig {
    rig_with(world(dreams, false), Fake::new(herdr, Some(data)))
}

fn start() -> Value {
    json!({ "cwd": "/repo", "surface": "terminal", "isInteractive": true, "startedAt": 0 })
}

fn pane_event() -> Value {
    json!({ "plugin": "csr-companion", "surface": "terminal", "component": "Pane",
        "requestId": "csr",
        "props": { "title": "CSR", "isFocused": true, "bodyColumns": 140, "placement": "dock" } })
}

impl Rig {
    /// The session start, and the background read and open it leaves.
    async fn started(&self) {
        run(&self.table, "start", &self.fx.cx, start()).await;
        self.fx
            .timers
            .advance(0., Arc::new(self.fx.host.clone()))
            .await;
    }

    async fn pane(&self) -> Value {
        let reply = run(&self.table, "pane", &self.fx.cx, pane_event()).await;
        reply.tree.expect("the pane draws a tree")
    }

    async fn press(&self, key: &str) {
        let e = json!({ "key": key, "requestId": "csr" });
        run(&self.table, PRESS, &self.fx.cx, e).await;
    }

    async fn csr(&self, args: &str) -> String {
        let reply = run(&self.table, "csr", &self.fx.cx, json!({ "args": args })).await;
        reply.answer.unwrap()["text"].as_str().unwrap().to_string()
    }

    fn opened(&self) -> Vec<Value> {
        self.fx.host.seen.lock().unwrap().opened.clone()
    }

    fn statuses(&self) -> Vec<Value> {
        self.fx.host.seen.lock().unwrap().statuses.clone()
    }

    fn configured(&self) -> Vec<Value> {
        self.fx.host.seen.lock().unwrap().configured.clone()
    }
}

/// The text of every Text node: its own string children, joined.
fn texts(tree: &Value, into: &mut Vec<String>) {
    if tree["type"] == "Text" {
        let own: String = tree["children"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        if !own.is_empty() {
            into.push(own);
        }
    }
    for child in tree["children"].as_array().into_iter().flatten() {
        if child.is_object() {
            texts(child, into);
        }
    }
}

fn all_texts(tree: &Value) -> Vec<String> {
    let mut into = Vec::new();
    texts(tree, &mut into);
    into
}

/// Whether some Text matches `pattern` (as the test kit's `find({ text: /.../ })` does).
fn has(tree: &Value, pattern: &str) -> bool {
    let re = Regex::new(pattern).unwrap();
    all_texts(tree).iter().any(|text| re.is_match(text))
}

fn has_exactly(tree: &Value, text: &str) -> bool {
    all_texts(tree).iter().any(|t| t == text)
}

fn buttons<'a>(tree: &'a Value, into: &mut Vec<&'a Value>) {
    if tree["type"] == "Button" {
        into.push(tree);
    }
    for child in tree["children"].as_array().into_iter().flatten() {
        if child.is_object() {
            buttons(child, into);
        }
    }
}

fn button_labels(tree: &Value) -> Vec<String> {
    let mut found = Vec::new();
    buttons(tree, &mut found);
    found
        .iter()
        .map(|b| b["props"]["label"].as_str().unwrap_or("").to_string())
        .collect()
}

#[tokio::test]
async fn session_start_in_herdr_no_status_line_old_pane_closed_new_split_watches_this() {
    let r = rig(true, true, shifts());
    r.started().await;

    // CSR pins no line under the prompt.
    assert!(r.statuses().contains(&Value::Null));
    assert!(!r.statuses().contains(&json!(shifts()["status"])));
    let runs = r.outside.runs();
    let herdr = |args: &[&str]| args.iter().map(|a| a.to_string()).collect::<Vec<_>>();
    assert!(runs.contains(
        &[
            vec!["/bin/herdr".to_string()],
            herdr(&["pane", "close", "w2:p2"])
        ]
        .concat()
    ));
    let split = r.outside.ran("split").unwrap();
    for want in ["--pane", "w2:p1", "--direction", "right", "--no-focus"] {
        assert!(
            split.iter().any(|a| a == want),
            "split lacks {want}: {split:?}"
        );
    }
    assert!(runs.contains(
        &[
            vec!["/bin/herdr".to_string()],
            herdr(&["pane", "rename", "w2:p9", "CSR"])
        ]
        .concat()
    ));
    let watcher = runs
        .iter()
        .find(|argv| argv.iter().any(|a| a == "--watch"))
        .unwrap();
    assert_eq!(watcher[..4], ["/bin/herdr", "pane", "run", "w2:p9"]);
    assert_eq!(watcher[4..7], [ENGINE, "companion", "pane"]);
    // A second click on a row types into Claude's pane.
    assert_eq!(
        watcher[watcher.len() - 4..],
        ["--sid", "sess-1", "--agent", "w2:p1"]
    );
    // The data was read in process, for this folder and session.
    assert_eq!(
        *r.outside.asked.lock().unwrap(),
        [("/repo".to_string(), "sess-1".to_string())]
    );
    assert!(r.opened().is_empty());
}

#[tokio::test]
async fn session_start_outside_herdr_opens_the_claude_code_pane_instead() {
    let r = rig(true, false, shifts());
    r.started().await;

    assert_eq!(r.opened(), [json!({ "id": "csr", "title": "CSR" })]);
    assert!(
        r.outside.runs().is_empty(),
        "no herdr command without herdr"
    );
}

#[tokio::test]
async fn outside_a_git_repo_no_status_text_no_pane_opened() {
    let r = rig(true, true, no_repo());
    r.started().await;

    assert!(!r.statuses().is_empty());
    assert!(r.statuses().iter().all(Value::is_null));
    assert!(r.outside.ran("split").is_none());
    assert!(r.opened().is_empty());
}

#[tokio::test]
async fn a_first_pane_py_call_that_times_out_still_leaves_the_refresh_and_the_pane_answer() {
    let r = rig_with(world(true, false), Fake::new(false, None));
    run(&r.table, "start", &r.fx.cx, start()).await;
    // The recurring read is set before any read; the TypeScript test had no clock to move.
    assert_eq!(r.fx.timers.everies(), [10. * 60. * 1000.]);
    r.fx.timers.advance(0., Arc::new(r.fx.host.clone())).await;
    assert!(r.opened().is_empty(), "nothing to open the pane with");
    assert_eq!(r.fx.timers.everies(), [10. * 60. * 1000.]);
}

#[tokio::test]
async fn the_pane_offers_dreams_while_they_are_off_its_1_asks_again_as_they_turn_on_in() {
    let r = rig(false, false, quiet());
    r.started().await;
    let pane = r.pane().await;

    assert!(
        !has(&pane, "CSR remembered"),
        "no badge line: what CSR did, in words"
    );
    assert!(has(&pane, "Dreams are off$"));
    assert!(has(&pane, "recalled your past work for Claude 2 times"));
    assert!(has(
        &pane,
        "carried 3 corrections of yours · told Claude about 1 code change"
    ));
    assert!(has(&pane, "covers all your Claude sessions"));
    r.press("dreams-on").await;
    // The user settings row reaches every window: one press only asks.
    assert_eq!(r.configured(), Vec::<Value>::new());
    let pane = r.pane().await;
    assert_eq!(
        button_labels(&pane),
        ["Press again: dreams on in all your sessions"]
    );
    r.press("dreams-on").await;
    assert_eq!(
        r.configured(),
        [json!({ "key": "csr-companion.dreams", "value": true })]
    );
    let pane = r.pane().await;
    assert!(has(&pane, "Dreams are on$"));
    assert!(button_labels(&pane).is_empty());
}

#[tokio::test]
async fn a_refused_config_row_leaves_the_offer_up_and_says_where_to_turn_dreams_on() {
    let r = rig_with(world(false, true), Fake::new(false, Some(quiet())));
    r.started().await;
    assert!(has(&r.pane().await, "Dreams are off$"));
    r.press("dreams-on").await;
    r.press("dreams-on").await;
    // The write was tried once, and the host refused it.
    assert_eq!(
        r.configured(),
        [json!({ "key": "csr-companion.dreams", "value": true })]
    );
    let pane = r.pane().await;
    assert!(has(&pane, r"Couldn't turn dreams on here. Use /config"));
    assert!(!button_labels(&pane).is_empty());
}

#[tokio::test]
async fn nothing_found_yet_no_empty_headings_and_the_pane_says_what_will_show_there() {
    let r = rig(true, false, quiet());
    r.started().await;
    let pane = r.pane().await;

    assert!(has(&pane, "What CSR can do here$"));
    assert!(!has(&pane, "Changed since your sessions$"));
    assert!(!has(&pane, "Change log$"));
    assert!(has(&pane, "Dreams are on$"));
    assert!(
        button_labels(&pane).is_empty(),
        "dreams on: nothing to offer"
    );
    assert!(has(
        &pane,
        "Code you worked on that a later commit changes shows here"
    ));
}

#[tokio::test]
async fn outside_git_the_pane_says_changed_code_cannot_show_there_not_that_none_changed() {
    let mut data = quiet();
    data["git"] = json!(false);
    let r = rig(true, false, data);
    r.started().await;
    let pane = r.pane().await;

    assert!(has(&pane, "What CSR can do here$"));
    assert!(has(&pane, "Not in git, so code changes cannot show here"));
    assert!(!has(&pane, "a later commit changes shows here"));
}

#[tokio::test]
async fn with_changes_found_the_pane_lists_them_and_no_longer_promises_them() {
    let r = rig(true, false, shifts());
    r.started().await;
    let pane = r.pane().await;

    assert!(has(&pane, "Changed since your sessions$"));
    assert!(has(&pane, "09-20 remove the dream statusline"));
    assert!(has(&pane, "Change log$"));
    assert!(!has(&pane, "a later commit changes shows here"));
}

#[tokio::test]
async fn with_dreams_off_the_pane_shows_the_dream_you_marked_helpful_as_its_sample() {
    let line = "What if the pre-commit hook reran a failed test once before blocking?";
    let mut data = quiet();
    data["sample"] = json!({ "line": line, "at": 1_791_651_060.0, "answer": "helpful",
        "project": "claude-self-reflect" });
    let r = rig(false, false, data);
    r.started().await;
    let pane = r.pane().await;

    assert!(has(&pane, "Dreams are off$"));
    assert!(has_exactly(
        &pane,
        "On 10-10, in claude-self-reflect, the dreamer asked this, and you marked it helpful:"
    ));
    assert!(has_exactly(&pane, &format!("“{line}”")));
    assert!(has(
        &pane,
        r#"^Turned on, it asks one "what if\.\.\.\?" like this"#
    ));
    assert!(!has(&pane, "^Turn them on, and while Claude works"));
    assert!(!button_labels(&pane).is_empty());
}

// ---- what the port adds: the routes, the retired status line, the person record

#[tokio::test]
async fn a_start_registers_csr_clears_the_status_line_and_names_no_statusline() {
    let r = rig(true, false, shifts());
    run(&r.table, "start", &r.fx.cx, start()).await;
    let seen = r.fx.host.seen.lock().unwrap();
    assert_eq!(seen.registered.len(), 1);
    let description = seen.registered[0]["description"].as_str().unwrap();
    assert!(description.contains("/csr setup"));
    assert!(description.contains("/csr dreams"));
    assert!(!description.contains("statusline"), "{description}");
    assert_eq!(seen.registered[0]["name"], "csr");
    assert!(seen.statuses.iter().all(Value::is_null) && !seen.statuses.is_empty());
}

#[tokio::test]
async fn a_start_that_is_not_a_person_records_it_and_does_nothing_else() {
    let r = rig(true, true, shifts());
    let e = json!({ "cwd": "/repo", "surface": "terminal", "isInteractive": false });
    run(&r.table, "start", &r.fx.cx, e).await;
    r.fx.timers.advance(0., Arc::new(r.fx.host.clone())).await;
    assert!(
        r.fx.host.calls().is_empty(),
        "no command, no status, no pane: {:?}",
        r.fx.host.calls()
    );
    assert!(r.fx.timers.everies().is_empty());
    assert!(r.outside.asked.lock().unwrap().is_empty());
    assert!(r.outside.runs().is_empty());
    run(&r.table, "start", &r.fx.cx, start()).await;
    assert_eq!(r.fx.host.seen.lock().unwrap().registered.len(), 1);
}

#[tokio::test]
async fn a_start_that_does_not_say_it_is_a_person_is_not_one() {
    // The TypeScript: `if (!e.isInteractive) return`.
    let r = rig(true, false, shifts());
    run(&r.table, "start", &r.fx.cx, json!({ "cwd": "/repo" })).await;
    r.fx.timers.advance(0., Arc::new(r.fx.host.clone())).await;
    assert!(r.fx.host.calls().is_empty(), "{:?}", r.fx.host.calls());
    assert!(r.fx.host.seen.lock().unwrap().registered.is_empty());
    assert!(r.fx.timers.everies().is_empty());
}

#[tokio::test]
async fn the_refresh_reads_again_every_ten_minutes_and_keeps_what_it_read() {
    let r = rig(true, false, shifts());
    r.started().await;
    assert_eq!(r.outside.asked.lock().unwrap().len(), 1);
    assert_eq!(
        r.fx.cx.store.get("sess-1", "data").unwrap().unwrap()["total"],
        2
    );
    *r.outside.data.lock().unwrap() = Some(quiet());
    r.fx.timers
        .advance(10. * 60. * 1000., Arc::new(r.fx.host.clone()))
        .await;
    assert_eq!(r.outside.asked.lock().unwrap().len(), 2);
    assert_eq!(
        r.fx.cx.store.get("sess-1", "data").unwrap().unwrap()["total"],
        0
    );
    // A read that fails leaves no data: the pane says it could not be read.
    *r.outside.data.lock().unwrap() = None;
    r.fx.timers
        .advance(10. * 60. * 1000., Arc::new(r.fx.host.clone()))
        .await;
    assert!(r.fx.cx.store.get("sess-1", "data").unwrap().is_none());
    assert!(has(&r.pane().await, "csr-engine could not be read."));
}

#[tokio::test]
async fn a_respawned_start_sets_the_refresh_again_and_opens_nothing() {
    let r = rig(true, true, shifts());
    let mut cx = r.fx.cx.clone();
    cx.respawn = true;
    run(&r.table, "start", &cx, start()).await;
    r.fx.timers.advance(0., Arc::new(r.fx.host.clone())).await;
    assert_eq!(r.fx.timers.everies(), [10. * 60. * 1000.]);
    assert!(r.outside.asked.lock().unwrap().is_empty());
    assert!(r.outside.runs().is_empty() && r.opened().is_empty());
    assert!(
        r.fx.host.seen.lock().unwrap().registered.is_empty(),
        "the registration outlived it"
    );
}

#[tokio::test]
async fn csr_statusline_answers_one_line_and_changes_nothing() {
    let r = rig(true, true, shifts());
    for args in ["statusline", "statusline on", "statusline off"] {
        let said = r.csr(args).await;
        assert!(said.contains("status line wrapper is gone"), "{said}");
        assert!(said.contains("masthead") && said.contains("hook install --apply"));
        assert_eq!(said.lines().count(), 1);
    }
    assert!(r.outside.runs().is_empty() && r.outside.asked.lock().unwrap().is_empty());
    assert!(r.opened().is_empty());
}

#[tokio::test]
async fn the_pane_does_not_offer_the_retired_status_line() {
    let r = rig(true, false, quiet());
    r.started().await;
    let pane = r.pane().await;
    assert!(!has(&pane, "(?i)statusline|status line"));
    assert!(has(&pane, "In herdr, this pane opens as a split"));
}

#[tokio::test]
async fn csr_dreams_says_the_pace_or_that_dreams_are_off() {
    let on = rig(true, false, shifts());
    let said = on.csr("dreams").await;
    assert!(
        said.starts_with("Dreams are on. 5 of 5 dreams left in this chapter;"),
        "{said}"
    );
    assert!(said.ends_with("No looks yet this session."));
    on.fx
        .cx
        .store
        .set(
            "sess-1",
            "pace",
            &json!({ "session": "sess-1", "claude": 100_000,
        "dreamer": 30_000, "looks": 1, "dreams": 1, "lastLook": 0 }),
        )
        .unwrap();
    let said = on.csr(" dreams please").await;
    assert!(
        said.contains("This session: 1 look, 1 dream, 30k tokens (30% of Claude's 100k)."),
        "{said}"
    );
    assert!(said.contains("A look ran over its share by 20k"));
    let off = rig(false, false, shifts());
    assert_eq!(
        off.csr("dreams").await,
        "Dreams are off. Turn them on in /config: csr-companion, Dreams."
    );
}

#[tokio::test]
async fn csr_setup_goes_to_the_walkthrough_and_the_default_refreshes_opens_and_counts() {
    let r = rig(true, false, shifts());
    let said = r.csr("setup").await;
    assert!(
        said.starts_with("CSR setup is above the prompt: 2 steps"),
        "{said}"
    );
    assert!(r.outside.asked.lock().unwrap().is_empty());

    let said = r.csr("").await;
    assert!(
        said.starts_with(
            "CSR pane opened (0 dreams this week, 1 changed since your sessions). Dreams are on."
        ),
        "{said}"
    );
    assert_eq!(r.opened(), [json!({ "id": "csr", "title": "CSR" })]);
    assert_eq!(r.outside.asked.lock().unwrap().len(), 1);

    let herdr = rig(true, true, shifts());
    let said = herdr.csr("anything else").await;
    assert!(
        said.starts_with("CSR pane opened beside this one (0 dreams this week"),
        "{said}"
    );
    assert!(herdr.opened().is_empty());

    let broken = rig_with(world(true, false), Fake::new(false, None));
    let said = broken.csr("").await;
    assert!(
        said.starts_with("CSR pane opened (csr-engine could not be read). "),
        "{said}"
    );
}

#[tokio::test]
async fn a_command_with_no_words_is_the_relays_liveness_probe() {
    let r = rig(true, false, shifts());
    let reply = run(&r.table, "csr", &r.fx.cx, json!({})).await;
    let said = reply.answer.unwrap()["text"].as_str().unwrap().to_string();
    assert_eq!(said, "csr relay ok pid=1 events=1");
}

#[tokio::test]
async fn a_press_that_is_not_the_dreams_offer_changes_nothing() {
    let r = rig(false, false, quiet());
    r.started().await;
    r.press("setup-dreams-1").await;
    r.press("nothing").await;
    assert_eq!(r.fx.cx.kept(|s: &mut PaneState| s.turned_on), None);
    assert_eq!(r.fx.host.seen.lock().unwrap().invalidated, 0);
}

#[tokio::test]
async fn the_infinite_session_and_the_shift_map_show_when_there_is_something() {
    let mut data = quiet();
    data["shift_map"] = json!({ "title": "Shift Map", "rows": ["Monday", "  a quiet row"] });
    let r = rig(true, false, data);
    r.started().await;
    let bare = r.pane().await;
    assert!(!has(&bare, "Infinite session$"));
    assert!(has(&bare, "Shift Map$") && has(&bare, "a quiet row"));
    r.fx.cx
        .store
        .set(
            "sess-1",
            "infinite",
            &json!({ "plan": "Compact at 181k.", "story": null }),
        )
        .unwrap();
    let pane = r.pane().await;
    assert!(has(&pane, "Infinite session$") && has_exactly(&pane, "Compact at 181k."));
}

#[test]
fn the_table_gives_pane_cmd_its_two_starts_the_command_the_pane_and_the_dreams_press() {
    let mut t = Table::new();
    register(&mut t);
    register_late(&mut t);
    assert_eq!(t.features("start"), ["pane_cmd", "pane_cmd"]);
    assert_eq!(t.features("csr"), ["pane_cmd"]);
    assert_eq!(t.features("pane"), ["pane_cmd"]);
    assert_eq!(t.features(PRESS), ["pane_cmd"]);
}
