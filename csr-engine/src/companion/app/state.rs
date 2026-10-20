//! What the features share: the store rows they all read the same way, the options, `show`.
//!
//! The TypeScript plugin keeps these in `register.tsx` (`strings`, `count`, `remember`, `answered`,
//! `show`, `update`, `configRow`) and its `options`. A feature that is the only one to use a helper
//! keeps it; what two or more use lives here.
//!
//! Store rows, as the TypeScript's keys map: a per-session atom (`view`, `setup`, ...) is the row
//! `(sid, key)`; a key that is not per session (`seen`, `known`, `answers`, `setup:answer:<step>`,
//! `setup:seen:<step>`) is the machine's row, `(MACHINE, key)`.

use crate::companion::{relay::dispatch::Ctx, store::MACHINE};
use anyhow::Result;
use serde_json::{json, Value};
use std::sync::Mutex;

/// How many ids and answers a list row keeps (register.tsx `KEPT`).
pub const KEPT: usize = 200;

/// The edges of the band where CSR compacts (compact.ts `BAND`, low and high).
pub const BAND_LOW: f64 = 180_589.;
pub const BAND_HIGH: f64 = 269_178.;

/// Tokens as the plugin writes them: `181k`, `1.2M` (compact.ts `k`).
pub fn k(tokens: f64) -> String {
    if tokens < 1e6 {
        return format!("{}k", (tokens / 1000.).round());
    }
    let millions = tokens / 1e6;
    // JavaScript's `toFixed` takes the larger of two equally near results; `{:.1}` takes the
    // even one.
    let tie = (millions * 4.).fract() == 0. && (millions * 4.) as u64 % 2 == 1;
    format!("{:.1}M", if tie { millions + 0.01 } else { millions })
}

/// What one `/config` write of `autoCompact` came to (compact.ts `settingOf`).
pub fn setting_of(value: &Value) -> &'static str {
    match value.as_str() {
        Some("off") => "off",
        Some("auto") => "auto",
        _ => "suggest",
    }
}

/// The strings a list row holds; anything else in it, or no row, is nothing.
pub fn strings(cx: &Ctx, sid: &str, key: &str) -> Vec<String> {
    match cx.store.get(sid, key) {
        Ok(Some(Value::Array(items))) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// Add `value` to the machine's list row `key` unless it is there, keeping the newest `kept`.
pub fn remember(cx: &Ctx, key: &str, value: &str, kept: usize) -> Result<()> {
    let mut list = strings(cx, MACHINE, key);
    if list.iter().any(|item| item == value) {
        return Ok(());
    }
    list.push(value.to_string());
    let from = list.len().saturating_sub(kept);
    cx.store.set(MACHINE, key, &json!(list[from..]))
}

/// Held while a row is read and written back: the TypeScript's `update` is atomic, and two
/// handlers of one engine can reach the same row between their awaits.
static ROWS: Mutex<()> = Mutex::new(());

/// Change the session's row `key` as one step: `change` gets what it holds and returns what it
/// should hold (`None`: no row). Returns the new value.
pub fn update(
    cx: &Ctx,
    key: &str,
    change: impl FnOnce(Option<Value>) -> Option<Value>,
) -> Result<Option<Value>> {
    let _held = ROWS.lock().unwrap_or_else(|e| e.into_inner());
    let was = cx.store.get(&cx.sid, key)?.filter(|value| !value.is_null());
    let now = change(was);
    match &now {
        Some(value) => cx.store.set(&cx.sid, key, value)?,
        None => cx.store.delete(&cx.sid, key)?,
    }
    Ok(now)
}

/// What a note came to, kept across sessions: the machine's `answers` list. A note shown is the
/// first row; the dreamer's package adds the other answers.
pub fn answered_shown(cx: &Ctx, note: &Value) -> Result<()> {
    let _held = ROWS.lock().unwrap_or_else(|e| e.into_inner());
    let mut list = match cx.store.get(MACHINE, "answers")? {
        Some(Value::Array(items)) => items,
        _ => Vec::new(),
    };
    list.push(json!({ "id": note["id"], "answer": "shown", "at": cx.clock.now() }));
    let from = list.len().saturating_sub(KEPT);
    cx.store.set(MACHINE, "answers", &json!(list[from..]))
}

/// Show a note unless another is up; true when it was shown. One note at a time: a view of
/// another kind than `asked` holds the place.
pub fn show(cx: &Ctx, note: &Value, tag: &str) -> Result<bool> {
    let shown_at = cx.clock.now();
    let mut shown = false;
    update(cx, "view", |current| {
        if current.as_ref().is_some_and(|view| view["kind"] != "asked") {
            return current;
        }
        shown = true;
        Some(json!({
            "kind": "offer", "note": note, "tag": tag, "shownAt": shown_at, "promptsSurvived": 0,
        }))
    })?;
    if shown {
        answered_shown(cx, note)?;
    }
    Ok(shown)
}

/// The plugin's `/config` rows, as `$.config.list()` answers them; none when it fails.
pub async fn config_rows(cx: &Ctx) -> Vec<Value> {
    match cx.host.call("config.list", vec![]).await {
        Ok(Value::Array(rows)) => rows,
        _ => Vec::new(),
    }
}

/// Whether `key` is this plugin's row `name`: `csr-companion.name`, or `csr-companion@<where>.name`
/// for a plugin loaded from a folder.
fn is_row(key: &str, name: &str) -> bool {
    let Some(rest) = key.strip_prefix("csr-companion") else {
        return false;
    };
    let rest = match rest.strip_prefix('@') {
        Some(tail) => match tail.split_once('.') {
            Some((place, after)) if !place.is_empty() => after,
            _ => return false,
        },
        None => match rest.strip_prefix('.') {
            Some(after) => after,
            None => return false,
        },
    };
    rest == name
}

/// The key of the plugin's `/config` row `name` (register.tsx `configRow`).
pub fn config_row(rows: &[Value], name: &str) -> String {
    rows.iter()
        .filter_map(|row| row["key"].as_str())
        .find(|key| is_row(key, name))
        .map_or_else(|| format!("csr-companion.{name}"), str::to_string)
}

/// What `is_person` asked of the session's surfaces, kept for the life of the process.
#[derive(Default)]
struct Surfaced(Option<bool>);

/// Whether a person is at this session (register.tsx `isPerson`, `person` at 875-876 and
/// 2386-2387): the session's start said so (`Session.interactive`, set from `e.isInteractive`
/// when that was a boolean), else it draws somewhere (`session.surfaces` is not empty), asked
/// once and kept. Every feature that acts only for a person calls this one.
pub async fn is_person(cx: &Ctx) -> bool {
    if let Some(interactive) = cx.session.lock().ok().and_then(|s| s.interactive) {
        return interactive;
    }
    if let Some(known) = cx.kept(|s: &mut Surfaced| s.0) {
        return known;
    }
    let drawn = matches!(
        cx.host.call("session.surfaces", vec![]).await,
        Ok(Value::Array(surfaces)) if !surfaces.is_empty()
    );
    cx.kept(|s: &mut Surfaced| *s.0.get_or_insert(drawn))
}

/// The person's choices the engine reads from `/config`. The relay passes no options to the
/// engine, so they are asked of `config.list` where a feature needs them.
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    /// Dreams are on unless the person turned them off.
    pub dreams: bool,
    /// `off`, `auto` or `suggest`.
    pub auto_compact: &'static str,
}

impl Options {
    pub fn of(rows: &[Value]) -> Self {
        let value = |name: &str| {
            rows.iter()
                .find(|row| row["key"].as_str().is_some_and(|key| is_row(key, name)))
                .map_or(Value::Null, |row| row["value"].clone())
        };
        Self {
            dreams: value("dreams") != json!(false),
            auto_compact: setting_of(&value("autoCompact")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_read_as_k_below_a_million_and_as_tenths_of_m_above() {
        assert_eq!(k(180_589.), "181k");
        assert_eq!(k(269_178.), "269k");
        assert_eq!(k(999_400.), "999k");
        assert_eq!(k(1_000_000.), "1.0M");
        assert_eq!(
            k(1_250_000.),
            "1.3M",
            "JavaScript's toFixed takes the larger of two"
        );
        assert_eq!(k(1_750_000.), "1.8M");
        assert_eq!(k(1_150_000.), "1.1M", "1.15 is a little under in binary");
    }

    #[test]
    fn a_config_row_is_found_by_its_plugin_prefix_and_defaults_to_the_plain_key() {
        let rows = [
            json!({ "key": "csr-companion@inline.dreams" }),
            json!({ "key": "other.dreams" }),
        ];
        assert_eq!(config_row(&rows, "dreams"), "csr-companion@inline.dreams");
        assert_eq!(
            config_row(&rows, "autoCompact"),
            "csr-companion.autoCompact"
        );
        assert_eq!(config_row(&[], "dreams"), "csr-companion.dreams");
        assert!(!is_row("csr-companion.dreamsX", "dreams"));
        assert!(!is_row("csr-companionX.dreams", "dreams"));
    }

    #[test]
    fn options_read_dreams_as_on_unless_false_and_the_setting_as_suggest_unless_named() {
        let none = Options::of(&[]);
        assert_eq!((none.dreams, none.auto_compact), (true, "suggest"));
        let rows = [
            json!({ "key": "csr-companion.dreams", "value": false }),
            json!({ "key": "csr-companion.autoCompact", "value": "auto" }),
        ];
        let set = Options::of(&rows);
        assert_eq!((set.dreams, set.auto_compact), (false, "auto"));
        assert_eq!(setting_of(&json!("nonsense")), "suggest");
        assert_eq!(setting_of(&json!("off")), "off");
    }
}
