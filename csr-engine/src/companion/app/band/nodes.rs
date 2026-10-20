//! The band's tree: plain `{ type, props, children }` nodes (the shape `pane_cmd/pane.rs`
//! documents), which the relay builds into elements and wires the buttons of to `/press`.

use super::{buttons_of, Button};
use crate::companion::app::setup_walk::Walk;
use serde_json::{json, Value};

/// CSR's mark where You should know draws its ✦.
const STAR: &str = "☾";

pub(super) fn node(kind: &str, props: Value, children: Vec<Value>) -> Value {
    json!({ "type": kind, "props": props, "children": children })
}

fn text(props: Value, children: Vec<Value>) -> Value {
    node("Text", props, children)
}

/// Whether a band holds a Button with a hotkey: it is asking the person something (`asks`).
pub(super) fn asks(tree: &Value) -> bool {
    if tree["type"] == "Button" && super::truthy(&tree["props"]["hotkey"]) {
        return true;
    }
    tree["children"]
        .as_array()
        .is_some_and(|kids| kids.iter().any(asks))
}

/// `ours` above what the bands below drew, when they drew anything.
pub(super) fn over(ours: Value, beneath: &Value) -> Value {
    if super::truthy(beneath) {
        node(
            "Box",
            json!({ "flexDirection": "column" }),
            vec![ours, json!("$beneath")],
        )
    } else {
        ours
    }
}

/// The mark, then `body`, in a row.
fn lead_row(body: Vec<Value>) -> Value {
    node(
        "Box",
        json!({ "flexDirection": "row", "alignItems": "flex-start" }),
        vec![
            node(
                "Box",
                json!({ "flexShrink": 0, "width": 2 }),
                vec![text(json!({ "color": "suggestion" }), vec![json!(STAR)])],
            ),
            text(json!({ "wrap": "wrap" }), body),
        ],
    )
}

fn warning(message: &str) -> Value {
    node(
        "Box",
        json!({ "marginLeft": 2 }),
        vec![text(
            json!({ "color": "warning", "wrap": "wrap" }),
            vec![json!(message)],
        )],
    )
}

/// The walkthrough's step: what it does, the choices on keys, the one in force marked
/// (`setupBand`).
pub(super) fn walk_node(walk: &Walk) -> Value {
    let mut lines = walk.lines.iter();
    let first = lines.next().cloned().unwrap_or_default();
    let mut kids = vec![lead_row(vec![
        text(
            json!({ "dimColor": true }),
            vec![json!(format!("{} · ", walk.heading))],
        ),
        json!(first),
    ])];
    for line in lines {
        kids.push(node(
            "Box",
            json!({ "marginLeft": 2 }),
            vec![text(
                json!({ "dimColor": true, "wrap": "wrap" }),
                vec![json!(line)],
            )],
        ));
    }
    if let Some(message) = walk.message.as_deref().filter(|m| !m.is_empty()) {
        kids.push(warning(message));
    }
    let mut choices: Vec<Value> = walk
        .choices
        .iter()
        .map(|choice| {
            node(
                "Box",
                json!({ "marginRight": 3 }),
                vec![node(
                    "Button",
                    json!({ "key": choice.key, "hotkey": choice.hotkey, "plain": true,
                        "label": choice.label }),
                    vec![],
                )],
            )
        })
        .collect();
    choices.push(text(
        json!({ "dimColor": true }),
        vec![json!("Typing skips it; /csr setup brings it back.")],
    ));
    kids.push(node(
        "Box",
        json!({ "marginLeft": 2, "flexWrap": "wrap" }),
        choices,
    ));
    node(
        "Box",
        json!({ "flexDirection": "column", "marginTop": 1 }),
        kids,
    )
}

/// The row of buttons (`buttons`).
fn buttons_row(row: &[Button]) -> Value {
    let drawn = row.iter().map(|b| {
        let mut props = json!({ "key": b.key, "hotkey": b.hotkey, "plain": true,
            "label": b.label });
        if b.dim {
            props["dimColor"] = json!(true);
        }
        if b.dismiss {
            props["role"] = json!("dismiss");
        }
        node(
            "Box",
            json!({ "marginRight": 3 }),
            vec![node("Button", props, vec![])],
        )
    });
    node(
        "Box",
        json!({ "marginLeft": 2, "flexWrap": "wrap" }),
        drawn.collect(),
    )
}

/// `12345` as `12,345` (`toLocaleString('en-US')` of a whole number).
fn thousands(n: f64) -> String {
    let digits = format!("{}", n.abs().round() as u64);
    let mut out = String::new();
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    if n < 0. {
        out.insert(0, '-');
    }
    out
}

/// The note: its offer, its explanation, or its question, with the buttons that answer it.
pub(super) fn note_node(shown: &Value) -> Value {
    let note = &shown["note"];
    let word = |name: &str| note[name].as_str().unwrap_or("");
    let dream = note["kind"] == "dream";
    let carried = if shown["carried"] == true {
        " · still open from before the compaction"
    } else {
        ""
    };
    let lead = |body: &str| {
        lead_row(vec![
            text(
                json!({ "dimColor": true }),
                vec![json!(format!(
                    "{}{carried} · ",
                    shown["tag"].as_str().unwrap_or("")
                ))],
            ),
            json!(body),
        ])
    };
    let buttons = buttons_row(&buttons_of(shown));
    let message = shown["message"].as_str().filter(|m| !m.is_empty());
    let column = |kids: Vec<Value>| {
        node(
            "Box",
            json!({ "flexDirection": "column", "marginTop": 1 }),
            kids,
        )
    };
    if note["kind"] == "term" || shown["kind"] == "offer" {
        // Coaching: one word, defined once in your CLAUDE.md, only if you press 1. An offer: the
        // note's line and its keys.
        let mut kids = vec![lead(word("line")), buttons];
        kids.extend(message.map(warning));
        return column(kids);
    }
    if shown["kind"] == "explained" {
        let mut body = vec![text(
            json!({ "bold": true, "wrap": "wrap" }),
            vec![json!(word("title"))],
        )];
        body.push(if dream {
            node("Markdown", json!({ "text": word("text") }), vec![])
        } else {
            text(json!({ "wrap": "wrap" }), vec![json!(word("text"))])
        });
        let sketch: Vec<&str> = (note["sketch"].as_array().into_iter().flatten())
            .filter_map(Value::as_str)
            .collect();
        if !sketch.is_empty() {
            body.push(node(
                "Box",
                json!({ "marginTop": 1 }),
                vec![node(
                    "Code",
                    json!({ "source": sketch.join("\n"), "language": "text",
                        "wrap": "truncate-end" }),
                    vec![],
                )],
            ));
        }
        let source = if dream {
            let tokens = note["tokens"].as_f64().unwrap_or(0.);
            format!(
                "From CSR's dreamer · this look {} tokens, mostly re-read from the prompt cache",
                thousands(tokens)
            )
        } else {
            let oid: String = word("oid").chars().take(7).collect();
            let commit = if oid.is_empty() {
                String::new()
            } else {
                format!(" · commit {oid}")
            };
            let session: String = word("conversation").chars().take(8).collect();
            format!("From CSR's memory · 0 model tokens{commit} · session {session}")
        };
        body.push(text(
            json!({ "dimColor": true, "wrap": "truncate-end" }),
            vec![json!(source)],
        ));
        if let Some(message) = message {
            body.push(text(
                json!({ "color": "warning", "wrap": "wrap" }),
                vec![json!(message)],
            ));
        }
        return column(vec![
            lead(word("line")),
            node(
                "Box",
                json!({ "flexDirection": "column", "marginLeft": 2, "marginTop": 1,
                    "marginBottom": 1 }),
                body,
            ),
            buttons,
        ]);
    }
    let asking = if dream {
        "Was that worth asking?"
    } else {
        "Was that worth showing?"
    };
    node(
        "Box",
        json!({ "flexDirection": "row", "marginTop": 1, "flexWrap": "wrap" }),
        vec![
            node(
                "Box",
                json!({ "flexShrink": 0, "width": 2 }),
                vec![text(json!({ "color": "suggestion" }), vec![json!(STAR)])],
            ),
            node(
                "Box",
                json!({ "marginRight": 3 }),
                vec![text(json!({ "dimColor": true }), vec![json!(asking)])],
            ),
            buttons,
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_whole_number_reads_with_commas_as_en_us_writes_it() {
        assert_eq!(thousands(0.), "0");
        assert_eq!(thousands(999.), "999");
        assert_eq!(thousands(41_000.), "41,000");
        assert_eq!(thousands(1_234_567.), "1,234,567");
    }

    #[test]
    fn a_band_asks_when_a_button_with_a_hotkey_is_anywhere_in_it() {
        let button = json!({ "type": "Button", "props": { "hotkey": "1" }, "children": [] });
        let plain = json!({ "type": "Button", "props": {}, "children": [] });
        assert!(asks(&node(
            "Box",
            json!({}),
            vec![node("Box", json!({}), vec![button])]
        )));
        assert!(!asks(&node("Box", json!({}), vec![plain, json!("text")])));
        assert!(!asks(&Value::Null));
    }
}
