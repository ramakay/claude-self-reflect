//! What the band's tests, and the tests of the features that feed it, share: a pretend machine
//! and pane, the seams made of them, and a way to draw the band and press its buttons by label.

use super::super::Seams;
use crate::companion::{
    app::{
        compactor::tests::kit::{FakeOutside, Setup},
        guard::{self, Machine, Ran},
        pane_cmd::{self, Outside},
    },
    relay::dispatch::{
        testing::{run, Fixture},
        Ctx, Table, PRESS,
    },
};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

pub const HOME: &str = "/home/u";
pub const CLAUDE_MD: &str = "/home/u/.claude/CLAUDE.md";

/// A machine with a disk of its own: what a test puts there, and what was written.
#[derive(Default)]
pub struct Computer {
    files: Mutex<HashMap<String, String>>,
    writes: Mutex<Vec<String>>,
    /// A file that is there and cannot be written.
    pub locked: Mutex<bool>,
}

impl Computer {
    pub fn put(&self, path: &str, text: &str) {
        self.files.lock().unwrap().insert(path.into(), text.into());
    }

    pub fn text(&self, path: &str) -> Option<String> {
        self.files.lock().unwrap().get(path).cloned()
    }

    pub fn writes(&self) -> Vec<String> {
        self.writes.lock().unwrap().clone()
    }
}

impl Machine for Computer {
    fn home(&self) -> Option<String> {
        Some(HOME.into())
    }

    fn exists(&self, path: &str) -> bool {
        self.files.lock().unwrap().contains_key(path)
    }

    fn read(&self, path: &str) -> Option<String> {
        self.text(path)
    }

    fn write(&self, path: &str, text: &str) -> bool {
        if *self.locked.lock().unwrap() {
            return false;
        }
        self.put(path, text);
        self.writes.lock().unwrap().push(path.into());
        true
    }

    fn keep(&self, path: &str, text: &str) -> bool {
        self.write(path, text)
    }

    fn variable(&self, _name: &str) -> Option<String> {
        None
    }

    fn git(
        &self,
        _dir: &str,
        _settings: &[String],
        _env: &[(String, String)],
        _args: &[&str],
    ) -> Option<Ran> {
        None
    }

    fn coach(&self, _now: f64) -> Vec<Value> {
        Vec::new()
    }
}

/// A pane outside that is not in herdr: the pane opens as Claude Code's own.
#[derive(Default)]
pub struct Panes {
    pub ran: Mutex<Vec<Vec<String>>>,
}

#[async_trait]
impl Outside for Panes {
    async fn shifts(&self, _cwd: &str, _sid: &str) -> Option<Value> {
        None
    }

    fn env(&self, _name: &str) -> Option<String> {
        None
    }

    async fn run(&self, argv: Vec<String>) -> Option<pane_cmd::Ran> {
        self.ran.lock().unwrap().push(argv);
        None
    }

    fn engine(&self) -> Option<String> {
        None
    }
}

/// The seams of a band under test, with the fakes kept to look at.
pub struct Gear {
    pub seams: Seams,
    pub machine: Arc<Computer>,
    pub panes: Arc<Panes>,
}

pub fn gear() -> Gear {
    let machine = Arc::new(Computer::default());
    let compact = Arc::new(FakeOutside::new(Setup::auto(0.)));
    let panes = Arc::new(Panes::default());
    let seams = Seams {
        compact,
        machine: machine.clone() as Arc<dyn guard::Machine>,
        pane: panes.clone(),
    };
    Gear {
        seams,
        machine,
        panes,
    }
}

/// What a band tree shows: its text, the labels of its buttons, and the keys they press with.
#[derive(Default, Debug)]
pub struct Drawn {
    pub texts: Vec<String>,
    pub buttons: Vec<(String, String)>,
    /// Whether the band drew anything at all (none: it left `beneath` as it was).
    pub drew: bool,
}

impl Drawn {
    pub fn of(tree: Option<&Value>) -> Self {
        let mut drawn = Drawn {
            drew: tree.is_some(),
            ..Drawn::default()
        };
        if let Some(tree) = tree {
            drawn.walk(tree);
        }
        drawn
    }

    fn walk(&mut self, tree: &Value) {
        if tree["type"] == "Button" {
            let (key, label) = (&tree["props"]["key"], &tree["props"]["label"]);
            if let (Some(key), Some(label)) = (key.as_str(), label.as_str()) {
                self.buttons.push((label.to_string(), key.to_string()));
            }
        }
        if tree["type"] == "Markdown" {
            if let Some(text) = tree["props"]["text"].as_str() {
                self.texts.push(text.to_string());
            }
        }
        for child in tree["children"].as_array().into_iter().flatten() {
            match child {
                Value::String(text) => self.texts.push(text.clone()),
                node => self.walk(node),
            }
        }
    }

    /// Whether some text matches `pattern` (a regular expression, as the TypeScript's find).
    pub fn has(&self, pattern: &str) -> bool {
        let re = regex::Regex::new(pattern).unwrap();
        self.texts.iter().any(|text| re.is_match(text))
    }

    /// Whether some text is exactly `text`.
    pub fn says(&self, text: &str) -> bool {
        self.texts.iter().any(|shown| shown == text)
    }

    pub fn has_button(&self, label: &str) -> bool {
        self.buttons.iter().any(|(shown, _)| shown == label)
    }

    pub fn key_of(&self, label: &str) -> Option<String> {
        let found = self.buttons.iter().find(|(shown, _)| shown == label);
        found.map(|(_, key)| key.clone())
    }

    pub fn labels(&self) -> Vec<String> {
        self.buttons
            .iter()
            .map(|(label, _)| label.clone())
            .collect()
    }
}

/// What `AbovePrompt` draws over `beneath` (none: nothing below, as the first band has it).
pub async fn draw(table: &Table, cx: &Ctx, beneath: Value) -> Option<Value> {
    let e = json!({ "component": "AbovePrompt", "props": {}, "beneath": beneath });
    run(table, "band", cx, e).await.tree
}

pub async fn band(table: &Table, cx: &Ctx) -> Drawn {
    Drawn::of(draw(table, cx, Value::Null).await.as_ref())
}

/// A press of the button labelled `label` as the band draws it now; false when none is.
pub async fn press(table: &Table, cx: &Ctx, label: &str) -> bool {
    let Some(key) = band(table, cx).await.key_of(label) else {
        return false;
    };
    press_key(table, cx, &key).await;
    true
}

pub async fn press_key(table: &Table, cx: &Ctx, key: &str) {
    run(table, PRESS, cx, json!({ "key": key, "requestId": "r1" })).await;
}

/// The fixture's session row `key`.
pub fn row(fx: &Fixture, key: &str) -> Option<Value> {
    fx.cx.store.get(&fx.cx.sid, key).unwrap()
}
