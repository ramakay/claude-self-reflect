//! What a file tool call touches, and the note about the file it may bring: port of
//! the removed `touch.ts` and of the TypeScript companion's `claimOffer`/`offerFor` (called from the
//! `tool.call` hook).
//!
//! A note about a symbol fires only when the symbol's current span meets a line the call touches,
//! so the note mode is given the call's own words (an Edit's `old_string`, a Write's content, a
//! Read's window), not just the file: one JSON payload, `touch_payload`. The TypeScript plugin
//! pipes it to `csr-engine companion pane --note FILE --touch -`; here `note_in_process` runs the
//! same code in this process. The `tool` entry is the first group of the TypeScript's handler and
//! rewrites nothing: it claims the offer, then asks for the note apart from the call.

use super::state::{remember, show, strings, KEPT};
use crate::companion::{
    db::Db,
    note,
    paths::{self, Paths},
    relay::{
        dispatch::{handler, Ctx, Reply, Table},
        host::BoxFuture,
    },
    store::MACHINE,
};
use serde::Serialize;
use serde_json::{json, Number, Value};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{Arc, Mutex},
};

/// Bytes: a call whose input is bigger names no symbol (the note mode reads `too_big`).
pub const TOUCH_MAX: u64 = note::TOUCH_MAX;
/// Calls remembered per file, so a long session does not grow without end.
pub const ASKED_MAX: usize = 200;

/// The tools whose call is about a file.
const FILE_TOOLS: [&str; 5] = ["Read", "Edit", "Write", "MultiEdit", "NotebookEdit"];

/// The words of the call the note mode reads. Fields are in the order the TypeScript sets them, so
/// `touch_digest` hashes the text `JSON.stringify` makes.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Touch {
    pub tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<Number>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<Number>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_string: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replace_all: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edits: Option<Vec<EditOf>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub too_big: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EditOf {
    pub old_string: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replace_all: Option<bool>,
}

fn text(value: &Value) -> Option<String> {
    value.as_str().map(str::to_string)
}

fn number(value: &Value) -> Option<Number> {
    value.as_number().cloned()
}

/// The words of the call the note mode reads, from the tool's own argument names. Nothing the call
/// writes beyond what it needs (an Edit's `new_string` stays here).
pub fn touch_of(call: &Value) -> Touch {
    let tool = match &call["tool"] {
        Value::String(tool) => tool.clone(),
        Value::Null => "undefined".to_string(),
        other => other.to_string(),
    };
    let mut touch = Touch {
        tool: tool.clone(),
        ..Touch::default()
    };
    match tool.as_str() {
        "Read" => {
            touch.offset = number(&call["offset"]);
            touch.limit = number(&call["limit"]);
        }
        "Edit" => {
            touch.old_string = text(&call["old_string"]);
            touch.replace_all = (call["replace_all"] == true).then_some(true);
        }
        "MultiEdit" if call["edits"].is_array() => {
            touch.edits = Some(
                call["edits"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|edit| {
                        Some(EditOf {
                            old_string: text(&edit["old_string"])?,
                            replace_all: (edit["replace_all"] == true).then_some(true),
                        })
                    })
                    .collect(),
            );
        }
        "Write" => touch.content = text(&call["content"]),
        _ => (),
    }
    touch
}

/// The tools whose call changes the file: the note mode needs the file as it was before.
pub fn changes_file(touch: &Touch) -> bool {
    matches!(touch.tool.as_str(), "Edit" | "MultiEdit" | "Write")
}

fn units(text: &str) -> impl Iterator<Item = u32> + '_ {
    text.encode_utf16().map(u32::from)
}

/// A short name for the call, to tell an identical second call from a different one: the
/// TypeScript's hash over the UTF-16 units of the call's JSON.
pub fn touch_digest(touch: &Touch) -> String {
    let body = serde_json::to_string(touch).unwrap_or_default();
    let mut hash: u32 = 5381;
    for unit in units(&body) {
        hash = hash.wrapping_mul(33) ^ unit;
    }
    format!("{}:{}:{}", touch.tool, units(&body).count(), base36(hash))
}

fn base36(mut n: u32) -> String {
    if n == 0 {
        return "0".into();
    }
    let mut digits = Vec::new();
    while n > 0 {
        digits.push(char::from_digit(n % 36, 36).unwrap_or('0'));
        n /= 36;
    }
    digits.iter().rev().collect()
}

/// The file as it was before the tool ran, as `touch_payload` is told of it.
#[derive(Clone, Debug, PartialEq)]
pub enum Before {
    /// The call is not one that changes the file.
    Unasked,
    /// It was not there, was not a regular file, or could not be read.
    Missing,
    Text(String),
}

/// What goes to the note mode: the call, and for the tools that change the file the file as it was
/// before. A payload past `TOUCH_MAX` is replaced by one that says so.
pub fn touch_payload(touch: &Touch, before: &Before) -> String {
    let mut whole = serde_json::to_value(touch).unwrap_or(Value::Null);
    match before {
        Before::Unasked => (),
        Before::Missing => whole["before"] = Value::Null,
        Before::Text(text) => whole["before"] = json!(text),
    }
    let whole = whole.to_string();
    if whole.len() as u64 <= TOUCH_MAX {
        whole
    } else {
        too_big_payload(touch)
    }
}

#[derive(Serialize)]
struct TooBig<'a> {
    tool: &'a str,
    too_big: bool,
}

/// The payload for a call the note mode is not given: its input, or the file it would change, is
/// past `TOUCH_MAX`.
pub fn too_big_payload(touch: &Touch) -> String {
    let said = TooBig {
        tool: &touch.tool,
        too_big: true,
    };
    serde_json::to_string(&said).unwrap_or_default()
}

/// What the note mode answered: the notes, and how many notes (shift and rule) there would have
/// been for any touch. An answer from before `--touch` is a plain list: every note is a candidate.
#[derive(Debug, PartialEq)]
pub struct Answer {
    pub notes: Vec<Value>,
    pub candidates: f64,
}

pub fn parse_answer(answer: &Value) -> Option<Answer> {
    if let Some(list) = answer.as_array() {
        return Some(Answer {
            notes: list.clone(),
            candidates: list.len() as f64,
        });
    }
    let notes = answer.get("notes")?.as_array()?;
    let candidates = answer["candidates"].as_f64().unwrap_or(notes.len() as f64);
    Some(Answer {
        notes: notes.clone(),
        candidates,
    })
}

/// The calls already put to the note mode for each file, so an identical call does not ask again.
#[derive(Default)]
pub struct Asked {
    calls: HashMap<String, HashSet<String>>,
    quiet: HashSet<String>,
}

impl Asked {
    /// True when the note mode is due for this call: the file is not quiet and this exact call was
    /// not asked. Marks it asked.
    pub fn claim(&mut self, file: &str, digest: &str) -> bool {
        if self.quiet.contains(file) {
            return false;
        }
        let calls = self.calls.entry(file.to_string()).or_default();
        if calls.contains(digest) {
            return false;
        }
        if calls.len() >= ASKED_MAX {
            calls.clear();
        }
        calls.insert(digest.to_string());
        true
    }

    /// The note mode found nothing to say about the file for any call: ask no more while this
    /// engine lives.
    pub fn silence(&mut self, file: &str) {
        self.quiet.insert(file.to_string());
        self.calls.remove(file);
    }
}

/// What the offer needs of the disk, so a test can say what is there and what was read.
pub trait Files: Send + Sync {
    /// `(is a regular file, size in bytes)`; none when it is not there.
    fn stat(&self, path: &str) -> Option<(bool, u64)>;
    fn read(&self, path: &str) -> Option<String>;
}

pub struct Disk;

impl Files for Disk {
    fn stat(&self, path: &str) -> Option<(bool, u64)> {
        let info = std::fs::metadata(path).ok()?;
        Some((info.is_file(), info.len()))
    }

    fn read(&self, path: &str) -> Option<String> {
        let bytes = std::fs::read(path).ok()?;
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }
}

/// Whether the note mode is due for this call, and if so the file as it was before the tool ran: a
/// tool that changes the file may be done before the note mode starts, so the text is read here,
/// before `next`. The size is asked first, so a file past `TOUCH_MAX` is never read into memory
/// ahead of the edit: `too_big`, and the note mode is told so instead.
#[derive(Debug, PartialEq)]
pub struct Due {
    pub before: Before,
    pub too_big: bool,
}

pub async fn claim_offer(
    files: &Arc<dyn Files>,
    asked: &Mutex<Asked>,
    file: &str,
    touch: &Touch,
) -> Option<Due> {
    let claimed = {
        let mut asked = asked.lock().unwrap_or_else(|e| e.into_inner());
        asked.claim(file, &touch_digest(touch))
    };
    if !claimed {
        return None;
    }
    let due = |before, too_big| Some(Due { before, too_big });
    if !changes_file(touch) {
        return due(Before::Unasked, false);
    }
    let (files, path) = (files.clone(), file.to_string());
    let seen = tokio::task::spawn_blocking(move || match files.stat(&path) {
        Some((true, size)) if size <= TOUCH_MAX => (true, false, files.read(&path)),
        Some((true, _)) => (true, true, None),
        _ => (false, false, None),
    })
    .await
    .ok()?;
    match seen {
        (true, true, _) => due(Before::Missing, true),
        (true, false, Some(text)) => due(Before::Text(text), false),
        _ => due(Before::Missing, false),
    }
}

/// Where the notes come from: `(file, sid, payload)`, answered with the note mode's JSON, or none
/// when it failed.
pub type NoteSource = Arc<dyn Fn(String, String, String) -> BoxFuture<Option<Value>> + Send + Sync>;

/// The note mode of `csr-engine companion pane --note FILE --sid SID --touch -`, run here: the
/// payload is what that command reads on stdin.
pub fn note_in_process(paths: &Paths, file: &str, sid: &str, payload: &str) -> Option<Value> {
    let path = paths::realpath(Path::new(file));
    let root = path
        .parent()
        .and_then(|p| paths::git(p, &["rev-parse", "--show-toplevel"]));
    let mut output = note::NotesOutput::default();
    if let Some(root) = root {
        let db = Db::open(paths).ok()?;
        let raw: Value = if payload.len() as u64 > TOUCH_MAX {
            Value::Null
        } else {
            serde_json::from_str(payload).unwrap_or_default()
        };
        let touch = note::touch_of(&raw, &path);
        output = note::notes_for(&db, paths, Path::new(&root), &path, sid, None, Some(&touch));
    }
    serde_json::to_value(output).ok()
}

/// The real source: the engine's own code, on a blocking thread.
pub fn in_process() -> NoteSource {
    Arc::new(|file, sid, payload| {
        Box::pin(async move {
            let asked = move || note_in_process(&Paths::from_env(), &file, &sid, &payload);
            tokio::task::spawn_blocking(asked).await.ok().flatten()
        })
    })
}

/// Ask for the note about `file` and show the first one not seen or known. `tag` is the band's
/// label: `You should know` for a Read, `Heads up` for the rest.
pub async fn offer_for(
    cx: &Ctx,
    source: &NoteSource,
    asked: &Mutex<Asked>,
    file: &str,
    tag: &str,
    touch: &Touch,
    due: &Due,
) {
    let payload = if due.too_big {
        too_big_payload(touch)
    } else {
        touch_payload(touch, &due.before)
    };
    let Some(answered) = source(file.to_string(), cx.sid.clone(), payload).await else {
        return;
    };
    let Some(answer) = parse_answer(&answered) else {
        return;
    };
    if answer.candidates == 0. {
        asked
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .silence(file);
    }
    let mut skip: HashSet<String> = strings(cx, MACHINE, "seen").into_iter().collect();
    skip.extend(strings(cx, MACHINE, "known"));
    let fresh = answer
        .notes
        .iter()
        .find(|note| !note["id"].as_str().is_some_and(|id| skip.contains(id)));
    let Some(note) = fresh else { return };
    if show(cx, note, tag).unwrap_or(false) {
        let _ = remember(cx, "seen", note["id"].as_str().unwrap_or(""), KEPT);
        let _ = cx.host.call("ui.invalidate", vec![]).await;
    }
}

/// The file a tool call names: `file_path`, else a notebook's `notebook_path`.
fn file_of(e: &Value) -> Option<&str> {
    e["file_path"]
        .as_str()
        .or_else(|| e["notebook_path"].as_str())
        .filter(|file| !file.is_empty())
}

/// The `tool` entry, with the disk and the note source it uses.
pub fn register_with(t: &mut Table, source: NoteSource, files: Arc<dyn Files>) {
    let asked = Arc::new(Mutex::new(Asked::default()));
    t.on(
        "tool",
        "touch",
        handler(move |cx, e| {
            let (source, files, asked) = (source.clone(), files.clone(), asked.clone());
            async move {
                // A subagent's reads raise no note; only the main loop's file tools do.
                let tool = e["tool"].as_str().unwrap_or("");
                let main_loop = e.get("agentId").is_none_or(Value::is_null);
                let Some(file) = file_of(&e).filter(|_| main_loop && FILE_TOOLS.contains(&tool))
                else {
                    return Ok(Reply::none());
                };
                let (file, touch) = (file.to_string(), touch_of(&e));
                let Some(due) = claim_offer(&files, &asked, &file, &touch).await else {
                    return Ok(Reply::none());
                };
                let tag = if tool == "Read" {
                    "You should know"
                } else {
                    "Heads up"
                };
                // Apart from the call: `next` does not wait for the note mode.
                cx.host.detach(Box::new(move |host| {
                    Box::pin(async move {
                        let cx = Ctx { host, ..cx };
                        offer_for(&cx, &source, &asked, &file, tag, &touch, &due).await;
                    })
                }));
                Ok(Reply::none())
            }
        }),
    );
}

pub fn register(t: &mut Table) {
    register_with(t, in_process(), Arc::new(Disk));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::companion::relay::{
        dispatch::testing::{fixture, run, until, Fixture},
        host::fake::World,
    };

    const EDITED: &str = "/repo/src/touch/edited.rs";
    /// `(file, sid, payload)` for each time the note mode was asked.
    type Asks = Arc<Mutex<Vec<(String, String, String)>>>;

    fn note() -> Value {
        json!({ "id": "n1", "kind": "shift", "file": "src/hooks/mod.rs", "title": "A note",
            "text": "changed in ca5655a", "conversation": "c1", "oid": "ca5655a1" })
    }

    /// A note source that records what it is asked and always answers `answer`.
    fn scripted(answer: Value) -> (NoteSource, Asks) {
        let asks: Asks = Arc::new(Mutex::new(Vec::new()));
        let seen = asks.clone();
        let source: NoteSource = Arc::new(move |file, sid, payload| {
            seen.lock().unwrap().push((file, sid, payload));
            let answer = answer.clone();
            Box::pin(async move { Some(answer) })
        });
        (source, asks)
    }

    /// A disk that is whatever a test says; it records the reads.
    #[derive(Default)]
    struct Pretend {
        files: HashMap<String, String>,
        dirs: HashSet<String>,
        sizes: HashMap<String, u64>,
        reads: Mutex<Vec<String>>,
    }

    impl Files for Pretend {
        fn stat(&self, path: &str) -> Option<(bool, u64)> {
            if self.dirs.contains(path) {
                return Some((false, 0));
            }
            let text = self.files.get(path)?;
            Some((true, *self.sizes.get(path).unwrap_or(&(text.len() as u64))))
        }

        fn read(&self, path: &str) -> Option<String> {
            self.reads.lock().unwrap().push(path.to_string());
            self.files.get(path).cloned()
        }
    }

    struct Rig {
        fx: Fixture,
        table: Table,
        asks: Asks,
        disk: Arc<Pretend>,
    }

    fn rig(answer: Value, disk: Pretend) -> Rig {
        let (source, asks) = scripted(answer);
        let disk = Arc::new(disk);
        let mut table = Table::new();
        register_with(&mut table, source, disk.clone());
        Rig {
            fx: fixture(World::default()),
            table,
            asks,
            disk,
        }
    }

    fn offering() -> Value {
        json!({ "notes": [note()], "candidates": 1 })
    }

    fn disk(files: &[(&str, &str)]) -> Pretend {
        Pretend {
            files: files
                .iter()
                .map(|(path, text)| (path.to_string(), text.to_string()))
                .collect(),
            ..Pretend::default()
        }
    }

    impl Rig {
        async fn call(&self, e: Value) {
            run(&self.table, "tool", &self.fx.cx, e).await;
        }

        fn said(&self) -> Vec<Value> {
            let asks = self.asks.lock().unwrap();
            asks.iter()
                .map(|(_, _, payload)| serde_json::from_str(payload).unwrap())
                .collect()
        }

        fn asked_files(&self) -> Vec<String> {
            self.asks
                .lock()
                .unwrap()
                .iter()
                .map(|a| a.0.clone())
                .collect()
        }

        async fn asked(&self, times: usize) {
            let asks = self.asks.clone();
            assert!(
                until(|| asks.lock().unwrap().len() >= times).await,
                "asked {times} times"
            );
        }

        /// What the band would draw: the `view` row.
        fn view(&self) -> Option<Value> {
            self.fx.cx.store.get("s", "view").unwrap()
        }

        async fn shown(&self) -> Value {
            let store = self.fx.cx.store.clone();
            assert!(
                until(|| store.get("s", "view").unwrap().is_some()).await,
                "a note is up"
            );
            self.view().unwrap()
        }
    }

    #[tokio::test]
    async fn an_edit_reaches_pane_py_with_its_old_string_and_the_file_as_it_was_on_stdin() {
        let rig = rig(offering(), disk(&[(EDITED, "fn a() {}\nfn b() { old }\n")]));
        let edit = json!({ "tool": "Edit", "file_path": EDITED, "old_string": "old",
            "new_string": "secret replacement", "replace_all": true });
        rig.call(edit).await;
        let view = rig.shown().await;
        let asks = rig.asks.lock().unwrap().clone();
        assert_eq!(asks.len(), 1);
        // The file and the session are all the note mode is told by name; the call is not in them.
        assert_eq!((asks[0].0.as_str(), asks[0].1.as_str()), (EDITED, "s"));
        assert!(!format!("{}{}", asks[0].0, asks[0].1).contains("old"));
        let payload: Value = serde_json::from_str(&asks[0].2).unwrap();
        assert_eq!(
            payload,
            json!({ "tool": "Edit", "old_string": "old", "replace_all": true,
                "before": "fn a() {}\nfn b() { old }\n" })
        );
        // What the tool writes is not the note mode's business.
        assert!(!asks[0].2.contains("secret replacement"));
        assert_eq!(
            (view["kind"].as_str(), view["tag"].as_str()),
            (Some("offer"), Some("Heads up"))
        );
        assert_eq!(view["note"]["id"], "n1");
    }

    #[tokio::test]
    async fn a_read_says_its_window_a_multiedit_only_where_its_edits_stand_a_write_its() {
        let rig = rig(
            offering(),
            disk(&[("/repo/a.rs", "x\ny\n"), ("/repo/b.rs", "q\n")]),
        );
        rig.call(json!({ "tool": "Read", "file_path": "/repo/a.rs", "offset": 70, "limit": 20 }))
            .await;
        rig.call(
            json!({ "tool": "MultiEdit", "file_path": "/repo/a.rs", "edits": [
            { "old_string": "x", "new_string": "NEW", "replace_all": true },
            { "old_string": "y", "new_string": "NEW2" }, { "new_string": "no old" }] }),
        )
        .await;
        fn write(file: &str, content: &str) -> Value {
            json!({ "tool": "Write", "file_path": file, "content": content })
        }
        rig.call(write("/repo/b.rs", "rewritten\n")).await;
        rig.call(write("/repo/new.rs", "fresh\n")).await;
        rig.call(
            json!({ "tool": "NotebookEdit", "notebook_path": "/repo/n.ipynb",
            "new_source": "x" }),
        )
        .await;
        rig.asked(5).await;
        assert_eq!(
            rig.said(),
            [
                json!({ "tool": "Read", "offset": 70, "limit": 20 }),
                json!({ "tool": "MultiEdit", "before": "x\ny\n",
                    "edits": [{ "old_string": "x", "replace_all": true }, { "old_string": "y" }] }),
                json!({ "tool": "Write", "content": "rewritten\n", "before": "q\n" }),
                json!({ "tool": "Write", "content": "fresh\n", "before": null }),
                json!({ "tool": "NotebookEdit" }),
            ]
        );
        assert_eq!(
            rig.asked_files(),
            [
                "/repo/a.rs",
                "/repo/a.rs",
                "/repo/b.rs",
                "/repo/new.rs",
                "/repo/n.ipynb"
            ]
        );
    }

    #[tokio::test]
    async fn a_call_whose_input_is_past_8_mb_is_passed_as_too_big_not_as_the_megabytes() {
        let rig = rig(offering(), Pretend::default());
        let content = "x".repeat(9 * 1024 * 1024);
        rig.call(json!({ "tool": "Write", "file_path": "/repo/big.rs", "content": content }))
            .await;
        rig.asked(1).await;
        assert_eq!(
            rig.asks.lock().unwrap()[0].2,
            r#"{"tool":"Write","too_big":true}"#
        );
    }

    #[tokio::test]
    async fn a_file_past_8_mb_is_never_read_before_the_edit_the_pane_is_told_too_big() {
        let big = "/repo/src/huge.rs";
        let mut pretend = disk(&[(big, "fn huge() {}\n")]);
        pretend.sizes.insert(big.into(), TOUCH_MAX + 1);
        let rig = rig(offering(), pretend);
        rig.call(json!({ "tool": "Write", "file_path": big, "content": "small" }))
            .await;
        rig.asked(1).await;
        assert_eq!(
            rig.asks.lock().unwrap()[0].2,
            r#"{"tool":"Write","too_big":true}"#
        );
        assert!(!rig.disk.reads.lock().unwrap().contains(&big.to_string()));
    }

    #[tokio::test]
    async fn a_file_at_the_limit_is_read_as_before_a_missing_file_or_a_directory_sends_before() {
        let (edge, small) = ("/repo/src/edge.rs", "/repo/src/small.rs");
        let mut pretend = disk(&[(edge, "edge\n"), (small, "small\n")]);
        pretend.sizes.insert(edge.into(), TOUCH_MAX);
        pretend.dirs.insert("/repo/src".into());
        let rig = rig(offering(), pretend);
        for (file, content) in [(edge, "a"), (small, "b"), ("/repo/src/missing.rs", "c")] {
            rig.call(json!({ "tool": "Write", "file_path": file, "content": content }))
                .await;
        }
        rig.call(
            json!({ "tool": "Edit", "file_path": "/repo/src", "old_string": "x",
            "new_string": "y" }),
        )
        .await;
        rig.asked(4).await;
        assert_eq!(
            rig.said(),
            [
                json!({ "tool": "Write", "content": "a", "before": "edge\n" }),
                json!({ "tool": "Write", "content": "b", "before": "small\n" }),
                json!({ "tool": "Write", "content": "c", "before": null }),
                // A directory is not a file to snapshot.
                json!({ "tool": "Edit", "old_string": "x", "before": null }),
            ]
        );
        let reads = rig.disk.reads.lock().unwrap().clone();
        assert_eq!(reads, [edge, small]);
    }

    #[tokio::test]
    async fn a_file_the_pane_had_nothing_to_say_about_is_not_asked_again_one_with_notes_is() {
        let rig = rig(json!({ "notes": [], "candidates": 0 }), Pretend::default());
        let edit = |old: &str| {
            json!({ "tool": "Edit", "file_path": "/repo/quiet.rs", "old_string": old,
                "new_string": "b" })
        };
        rig.call(edit("a")).await;
        rig.asked(1).await;
        tokio::task::yield_now().await; // its answer has marked the file quiet
        rig.call(edit("c")).await;
        rig.call(json!({ "tool": "Read", "file_path": "/repo/quiet.rs", "offset": 3, "limit": 4 }))
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(rig.asks.lock().unwrap().len(), 1);
        // Another file is its own question.
        rig.call(json!({ "tool": "Read", "file_path": "/repo/other.rs" }))
            .await;
        rig.asked(2).await;
        assert_eq!(rig.asks.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn an_identical_second_call_does_not_spawn_the_pane_again_a_different_call_on_a() {
        let rig = rig(offering(), Pretend::default());
        let edit = json!({ "tool": "Edit", "file_path": "/repo/busy.rs", "old_string": "a",
            "new_string": "b" });
        rig.call(edit.clone()).await;
        rig.asked(1).await;
        // The same lines asked about: the same call, whatever it writes.
        rig.call(
            json!({ "tool": "Edit", "file_path": "/repo/busy.rs", "old_string": "a",
            "new_string": "something else" }),
        )
        .await;
        rig.call(edit.clone()).await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(rig.asks.lock().unwrap().len(), 1);
        rig.call(
            json!({ "tool": "Edit", "file_path": "/repo/busy.rs", "old_string": "z",
            "new_string": "b" }),
        )
        .await;
        rig.call(
            json!({ "tool": "Read", "file_path": "/repo/busy.rs", "offset": 1,
            "limit": 30 }),
        )
        .await;
        rig.asked(3).await;
        let said: Vec<Value> = rig
            .said()
            .iter()
            .map(|touch| touch.get("old_string").unwrap_or(&touch["limit"]).clone())
            .collect();
        assert_eq!(said, [json!("a"), json!("z"), json!(30)]);
    }

    #[tokio::test]
    async fn a_pane_from_before_touch_answering_a_bare_list_still_offers_its_note() {
        let rig = rig(json!([note()]), Pretend::default());
        rig.call(
            json!({ "tool": "Read", "file_path": "/repo/old-pane.rs", "offset": 1,
            "limit": 5 }),
        )
        .await;
        let view = rig.shown().await;
        assert_eq!(view["note"]["text"], "changed in ca5655a");
        assert_eq!(view["tag"], "You should know");
        assert_eq!(rig.asks.lock().unwrap().len(), 1);
    }

    // Not in the TypeScript's tests by name: the rest of what the module does.

    #[tokio::test]
    async fn a_subagents_call_and_a_tool_that_is_not_a_file_tool_ask_no_note() {
        let rig = rig(offering(), Pretend::default());
        rig.call(json!({ "tool": "Read", "file_path": "/repo/a.rs", "agentId": "sub-1" }))
            .await;
        rig.call(json!({ "tool": "Bash", "command": "ls", "file_path": "/repo/a.rs" }))
            .await;
        rig.call(json!({ "tool": "Read" })).await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(rig.asks.lock().unwrap().is_empty());
        assert!(rig.view().is_none());
    }

    #[tokio::test]
    async fn a_note_shown_or_known_is_not_offered_again_and_one_up_holds_the_place() {
        let rig = rig(offering(), Pretend::default());
        let cx = &rig.fx.cx;
        cx.store.set(MACHINE, "known", &json!(["n1"])).unwrap();
        rig.call(json!({ "tool": "Read", "file_path": "/repo/a.rs" }))
            .await;
        rig.asked(1).await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(rig.view().is_none(), "Knew this already keeps it away");
        cx.store.set(MACHINE, "known", &json!([])).unwrap();
        rig.call(json!({ "tool": "Read", "file_path": "/repo/b.rs" }))
            .await;
        rig.shown().await;
        assert_eq!(cx.store.get(MACHINE, "seen").unwrap(), Some(json!(["n1"])));
        let answers = cx.store.get(MACHINE, "answers").unwrap().unwrap();
        assert_eq!(answers[0]["answer"], "shown");
        // Seen now: the next file's offer finds nothing new.
        let before = rig.view();
        rig.call(json!({ "tool": "Read", "file_path": "/repo/c.rs" }))
            .await;
        rig.asked(3).await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(rig.view(), before);
        assert_eq!(
            cx.store
                .get(MACHINE, "answers")
                .unwrap()
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn the_digest_is_the_typescripts_for_the_same_call() {
        let digest = |call: Value| touch_digest(&touch_of(&call));
        let read = json!({ "tool": "Read", "offset": 1, "limit": 30 });
        assert_eq!(digest(read), "Read:37:t09jll");
        let edit = json!({ "tool": "Edit", "old_string": "a", "replace_all": true });
        assert_eq!(digest(edit), "Edit:51:hrqyl1");
        let write = json!({ "tool": "Write", "content": "héllo 𝒳\n" });
        assert_eq!(digest(write), "Write:39:12lxmft");
        let multi = json!({ "tool": "MultiEdit", "edits": [
            { "old_string": "x", "replace_all": true }, { "old_string": "y" }] });
        assert_eq!(digest(multi), "MultiEdit:87:eoh2d0");
        assert_eq!(
            digest(json!({ "tool": "NotebookEdit" })),
            "NotebookEdit:23:vvtt1g"
        );
    }

    #[test]
    fn a_touch_keeps_only_what_the_note_mode_reads() {
        let t = touch_of(
            &json!({ "tool": "Edit", "old_string": "o", "new_string": "n",
            "replace_all": false, "file_path": "/f" }),
        );
        assert_eq!(
            t,
            Touch {
                tool: "Edit".into(),
                old_string: Some("o".into()),
                ..Touch::default()
            }
        );
        assert!(changes_file(&t) && !changes_file(&touch_of(&json!({ "tool": "Read" }))));
        let read = touch_of(&json!({ "tool": "Read", "offset": "7", "limit": 2.5 }));
        assert_eq!(
            read.offset, None,
            "a window that is not a number is not one"
        );
        assert_eq!(read.limit, Number::from_f64(2.5));
        let multi = touch_of(&json!({ "tool": "MultiEdit", "edits": "nope" }));
        assert_eq!(multi.edits, None);
        assert_eq!(touch_of(&json!({})).tool, "undefined");
    }

    #[test]
    fn the_payload_carries_before_only_when_asked_and_is_swapped_when_past_the_limit() {
        let t = touch_of(&json!({ "tool": "Write", "content": "c" }));
        let parsed = |before: &Before| -> Value {
            serde_json::from_str(&touch_payload(&t, before)).unwrap()
        };
        assert_eq!(
            parsed(&Before::Unasked),
            json!({ "tool": "Write", "content": "c" })
        );
        assert_eq!(parsed(&Before::Missing)["before"], Value::Null);
        assert_eq!(parsed(&Before::Text("t".into()))["before"], "t");
        let huge = Before::Text("x".repeat(TOUCH_MAX as usize));
        assert_eq!(
            touch_payload(&t, &huge),
            r#"{"tool":"Write","too_big":true}"#
        );
    }

    #[test]
    fn an_answer_is_a_list_or_notes_with_a_count_and_nothing_else() {
        let n = || json!({ "id": "a" });
        let got = parse_answer(&json!([n(), n()])).unwrap();
        assert_eq!((got.notes.len(), got.candidates), (2, 2.));
        let got = parse_answer(&json!({ "notes": [n()], "candidates": 5 })).unwrap();
        assert_eq!((got.notes.len(), got.candidates), (1, 5.));
        let got = parse_answer(&json!({ "notes": [n()], "candidates": "x" })).unwrap();
        assert_eq!(
            got.candidates, 1.,
            "a count that is no number is the notes' own"
        );
        assert_eq!(parse_answer(&json!({ "notes": "x" })), None);
        assert_eq!(parse_answer(&json!("x")), None);
        assert_eq!(parse_answer(&Value::Null), None);
    }

    #[test]
    fn asked_forgets_a_files_calls_at_the_limit_and_a_silenced_file_stays_silent() {
        let mut asked = Asked::default();
        assert!(asked.claim("f", "a"));
        assert!(!asked.claim("f", "a"));
        assert!(asked.claim("g", "a"));
        for i in 1..ASKED_MAX {
            assert!(asked.claim("f", &format!("d{i}")));
        }
        assert!(
            asked.claim("f", "next"),
            "at the limit the file starts over"
        );
        assert!(asked.claim("f", "a"), "and what it forgot is asked again");
        asked.silence("f");
        assert!(!asked.claim("f", "never"));
        assert!(asked.claim("g", "b"));
    }

    #[test]
    fn the_note_mode_in_process_says_nothing_of_a_file_outside_any_repository() {
        let home = tempfile::tempdir().unwrap();
        let folder = tempfile::tempdir().unwrap();
        let file = folder.path().join("loose.rs");
        std::fs::write(&file, "fn a() {}\n").unwrap();
        let paths = Paths::new(home.path().to_path_buf());
        let said = note_in_process(&paths, file.to_str().unwrap(), "s", r#"{"tool":"Read"}"#);
        assert_eq!(said, Some(json!({ "notes": [], "candidates": 0 })));
        assert!(
            !home.path().join(".claude-self-reflect").exists(),
            "no database made"
        );
    }
}
