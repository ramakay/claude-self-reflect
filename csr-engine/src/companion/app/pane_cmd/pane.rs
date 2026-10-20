//! The pane's cards, drawn in the herdr pane's paper: a dark bar for the
//! counts, then a card per section, each in its own ink. The tree is plain data (`{ type, props,
//! children }`); the relay builds elements from it and wires each Button to `/press`.
//!
//! The status line is retired, so the "What CSR can do here" card no longer offers it.

use serde_json::{json, Map, Value};

const BAR: &str = "#2f3a48";
const BAR_LOW: &str = "#0e0f11";
const BAR_TEXT: &str = "#eef5fa";
const BAR_LABEL: &str = "#939ba5";
const TEXT: &str = "#17191c";
const QUIET: &str = "#5e5e59";
const FAINT: &str = "#888781";

const RECALL: &str = "#248363";
const CTX: &str = "#2c5aa0";

/// A section's colours: its ink, the heading's ground, the rows' ground, and its glyph.
struct Look {
    ink: &'static str,
    head: &'static str,
    row: &'static str,
    glyph: &'static str,
}

const DREAMS: Look = Look {
    ink: "#986c00",
    head: "#ece6d4",
    row: "#f1ecdf",
    glyph: "☾",
};
const INFINITE: Look = Look {
    ink: "#2c5aa0",
    head: "#e6ebf0",
    row: "#edf0f3",
    glyph: "♻",
};
const CAN: Look = Look {
    ink: "#2c5aa0",
    head: "#e6ebf0",
    row: "#edf0f3",
    glyph: "§",
};
const CHANGED: Look = Look {
    ink: "#5e5e59",
    head: "#ebebe9",
    row: "#f0f0ee",
    glyph: "~",
};

/// Where the dreams offer stands (the TypeScript companion's `turnedOn`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TurnedOn {
    Asked,
    On,
    Refused,
}

struct Part {
    text: String,
    color: Option<&'static str>,
    bold: bool,
}

fn part(text: impl Into<String>) -> Part {
    Part {
        text: text.into(),
        color: None,
        bold: false,
    }
}

impl Part {
    fn color(mut self, color: &'static str) -> Self {
        self.color = Some(color);
        self
    }
    fn bold(mut self) -> Self {
        self.bold = true;
        self
    }
}

/// One item of a card: text wrapped under its indent (or cut at the edge), with a mark in its own
/// column.
#[derive(Default)]
struct Item {
    text: String,
    color: Option<&'static str>,
    bold: bool,
    cut: bool,
    mark: Option<(&'static str, &'static str)>,
}

fn item(text: impl Into<String>) -> Item {
    Item {
        text: text.into(),
        ..Item::default()
    }
}

impl Item {
    fn color(mut self, color: &'static str) -> Self {
        self.color = Some(color);
        self
    }
    fn bold(mut self) -> Self {
        self.bold = true;
        self
    }
    fn cut(mut self) -> Self {
        self.cut = true;
        self
    }
    fn mark(mut self, glyph: &'static str, ink: &'static str) -> Self {
        self.mark = Some((glyph, ink));
        self
    }
}

/// JavaScript's `String.length`: UTF-16 code units.
fn ulen(text: &str) -> usize {
    text.encode_utf16().count()
}

fn pad_end(text: &str, width: usize) -> String {
    format!("{text}{}", " ".repeat(width.saturating_sub(ulen(text))))
}

/// `text.slice(from)` for text whose characters are single code units (dates, ids).
fn from(text: &str, at: usize) -> String {
    text.chars().skip(at).collect()
}

fn upto(text: &str, to: usize) -> String {
    text.chars().take(to).collect()
}

/// Words wrapped to a width, as the pane's rows are drawn; a word longer than the width is cut.
pub fn wrapped(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let piece: String = if ulen(word) > width {
            let mut cut = String::new();
            for c in word.chars() {
                if ulen(&cut) + c.len_utf16() > width {
                    break;
                }
                cut.push(c);
            }
            cut
        } else {
            word.to_string()
        };
        if !line.is_empty() && ulen(&line) + 1 + ulen(&piece) > width {
            lines.push(std::mem::take(&mut line));
            line = piece;
        } else if line.is_empty() {
            line = piece;
        } else {
            line = format!("{line} {piece}");
        }
    }
    lines.push(line);
    lines
}

fn node(kind: &str, props: Map<String, Value>, children: Vec<Value>) -> Value {
    json!({ "type": kind, "props": props, "children": children })
}

fn text_node(props: Value, text: &str) -> Value {
    let props = props.as_object().cloned().unwrap_or_default();
    node("Text", props, vec![json!(text)])
}

/// One row of the pane: its parts in their inks, on one ground, filled to the pane's width.
fn paint(cols: usize, back: &str, parts: Vec<Part>) -> Value {
    let used: usize = parts.iter().map(|p| ulen(&p.text)).sum();
    let mut kids: Vec<Value> = parts
        .iter()
        .map(|p| {
            let mut props = json!({ "backgroundColor": back });
            if let Some(color) = p.color {
                props["color"] = json!(color);
            }
            if p.bold {
                props["bold"] = json!(true);
            }
            text_node(props, &p.text)
        })
        .collect();
    // The engine trims a row's trailing spaces: a block in the ground's own colour keeps the fill.
    let fill = format!("{}█", " ".repeat(cols.saturating_sub(used + 1)));
    kids.push(text_node(
        json!({ "backgroundColor": back, "color": back }),
        &fill,
    ));
    let props = json!({ "backgroundColor": back, "wrap": "truncate-end" });
    node("Text", props.as_object().cloned().unwrap_or_default(), kids)
}

fn column(margin_top: bool, children: Vec<Value>) -> Value {
    let mut props = json!({ "flexDirection": "column" });
    if margin_top {
        props["marginTop"] = json!(1);
    }
    node(
        "Box",
        props.as_object().cloned().unwrap_or_default(),
        children,
    )
}

/// A card: its heading, then each item wrapped under its indent; a mark sits in a column of its
/// own.
fn card(cols: usize, look: &Look, title: &str, meta: &str, items: Vec<Item>) -> Value {
    let left = format!(" {}  {title}", look.glyph);
    let gap = if meta.is_empty() {
        String::new()
    } else {
        let room = cols.saturating_sub(ulen(&left) + ulen(meta) + 1);
        format!("{}{meta}", " ".repeat(room.max(1)))
    };
    let mut rows = vec![paint(
        cols,
        look.head,
        vec![part(left).color(look.ink).bold(), part(gap).color(QUIET)],
    )];
    for it in items {
        let indent = if it.mark.is_some() { 7 } else { 4 };
        let lines = if it.cut {
            vec![it.text.clone()]
        } else {
            wrapped(&it.text, cols.saturating_sub(indent + 1))
        };
        for (i, text) in lines.into_iter().enumerate() {
            let mut lead = vec![part("    ")];
            if let Some((glyph, ink)) = it.mark {
                let mark = if i > 0 {
                    "   ".to_string()
                } else {
                    pad_end(glyph, 3)
                };
                lead.push(part(mark).color(ink).bold());
            }
            let mut body = part(text).color(it.color.unwrap_or(TEXT));
            body.bold = it.bold;
            lead.push(body);
            rows.push(paint(cols, look.row, lead));
        }
    }
    column(true, rows)
}

/// The counts, on the herdr pane's dark masthead.
fn bar(cols: usize, back: &str, label: &str, value: &str) -> Vec<Value> {
    wrapped(value, cols.saturating_sub(15))
        .into_iter()
        .enumerate()
        .map(|(i, text)| {
            let label = pad_end(if i > 0 { "" } else { label }, 13);
            paint(
                cols,
                back,
                vec![
                    part(format!(" {label} ")).color(BAR_LABEL),
                    part(text).color(BAR_TEXT),
                ],
            )
        })
        .collect()
}

fn times(n: f64, one: &str, many: &str) -> String {
    format!("{} {}", js_number(n), if n == 1. { one } else { many })
}

/// A number as a template string prints it.
fn js_number(n: f64) -> String {
    if n.fract() == 0. && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// A JSON value as `${value}` prints it.
fn shown(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Number(n) => js_number(n.as_f64().unwrap_or(0.)),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn str_of(value: &Value) -> &str {
    value.as_str().unwrap_or("")
}

/// A non-zero count: JavaScript's truthy number.
fn count(value: &Value) -> Option<f64> {
    value.as_f64().filter(|n| *n != 0. && !n.is_nan())
}

fn mark_of(verb: &str) -> &'static str {
    match verb {
        "changed" => "~",
        "removed" => "-",
        "came back" => "+",
        _ => "",
    }
}

fn symbol_or<'a>(symbol: &'a Value, otherwise: &'a str) -> &'a str {
    symbol
        .as_str()
        .unwrap_or(if symbol.is_null() { otherwise } else { "" })
}

fn stamp(at: f64, format: &str) -> String {
    chrono::DateTime::from_timestamp_millis((at * 1000.) as i64)
        .map(|date| date.format(format).to_string())
        .unwrap_or_default()
}

/// The change log's commits, each with its rows, in the order they first appear.
fn by_commit(shifts: &[Value]) -> Vec<(String, Vec<&Value>)> {
    let mut groups: Vec<(String, Vec<&Value>)> = Vec::new();
    for shift in shifts {
        let key = format!(
            "{} {} {}",
            from(str_of(&shift["date"]), 5),
            upto(str_of(&shift["oid"]), 7),
            str_of(&shift["subject"])
        );
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, group)) => group.push(shift),
            None => groups.push((key, vec![shift])),
        }
    }
    groups
}

fn list(value: &Value) -> &[Value] {
    value.as_array().map_or(&[], Vec::as_slice)
}

/// The dreams card while dreams are off, with the press under it: your own dream, when there is
/// one, says what dreams do better than a description can.
fn dreams_offer(cols: usize, data: &Value, turned: Option<TurnedOn>) -> (Value, Value) {
    let sample = &data["sample"];
    let has = sample.is_object();
    let mut items = Vec::new();
    if has {
        let project = match sample["project"].as_str().filter(|p| !p.is_empty()) {
            Some(project) => format!(", in {project},"),
            None => String::new(),
        };
        let how = if sample["answer"] == "helpful" {
            "marked it helpful"
        } else {
            "handed it to Claude"
        };
        let when = stamp(sample["at"].as_f64().unwrap_or(0.), "%m-%d");
        items.push(
            item(format!(
                "On {when}{project} the dreamer asked this, and you {how}:"
            ))
            .color(FAINT),
        );
        items.push(item(format!("“{}”", str_of(&sample["line"]))).bold());
        items.push(item(""));
    }
    let share = "its spend stays within 1 token for every 10 Claude spends. A look that runs over \
        is paid back before the next.";
    let reason = "when your past sessions give it a reason. It looks only when CSR has something \
        new, and checks before each look that";
    items.push(
        item(if has {
            format!(
                "Turned on, it asks one \"what if...?\" like this while Claude works, \
                {reason} {share}"
            )
        } else {
            format!(
                "Turn them on, and while Claude works a side agent asks one \"what if...?\" \
                {reason} {share}"
            )
        })
        .color(QUIET),
    );
    items.push(
        item("The switch is in your user settings: it covers all your Claude sessions.")
            .color(QUIET),
    );
    let dreams = card(cols, &DREAMS, "Dreams are off", "off", items);
    // The press sits under the card, on the terminal's own ground, where the engine draws its
    // hotkey.
    let press = if turned == Some(TurnedOn::Asked) {
        "Press again: dreams on in all your sessions"
    } else {
        "Turn dreams on"
    };
    let mut kids = vec![node(
        "Button",
        json!({ "key": "dreams-on", "hotkey": "1", "plain": true, "label": press })
            .as_object()
            .cloned()
            .unwrap_or_default(),
        vec![text_node(
            json!({ "bold": true, "color": "suggestion" }),
            press,
        )],
    )];
    if turned == Some(TurnedOn::Refused) {
        kids.push(text_node(
            json!({ "color": "warning", "wrap": "wrap" }),
            "Couldn't turn dreams on here. Use /config: csr-companion, Dreams.",
        ));
    }
    let offer = node(
        "Box",
        json!({ "flexDirection": "column", "paddingLeft": 4 })
            .as_object()
            .cloned()
            .unwrap_or_default(),
        kids,
    );
    (dreams, offer)
}

/// The pane for `data` (the session's shifts data; none: it could not be read), `lasting` (the
/// infinite session row), `cols` wide.
pub fn render(
    data: Option<&Value>,
    lasting: Option<&Value>,
    dreams_on: bool,
    turned: Option<TurnedOn>,
    cols: usize,
) -> Value {
    let Some(data) = data.filter(|d| d.is_object()) else {
        return text_node(json!({ "dimColor": true }), "csr-engine could not be read.");
    };
    let cols = cols.max(40);

    let recalled = data["recalled"].as_f64().unwrap_or(0.);
    let mut week = Vec::new();
    if let Some(n) = count(&data["corrections_carried_7d"]) {
        week.push(format!(
            "carried {} of yours",
            times(n, "correction", "corrections")
        ));
    }
    if let Some(n) = count(&data["shifts_told_7d"]) {
        week.push(format!(
            "told Claude about {}",
            times(n, "code change", "code changes")
        ));
    }
    let session_line = if recalled != 0. {
        format!(
            "recalled your past work for Claude {}",
            times(recalled, "time", "times")
        )
    } else {
        "nothing recalled yet; CSR looks on each prompt".to_string()
    };
    let mut counts = bar(cols, BAR, "THIS SESSION", &session_line);
    if !week.is_empty() {
        counts.extend(bar(cols, BAR_LOW, "THIS WEEK", &week.join(" · ")));
    }
    let counts = column(false, counts);

    let mut offer = None;
    let dreams = if !dreams_on && turned != Some(TurnedOn::On) {
        let (dreams, press) = dreams_offer(cols, data, turned);
        offer = Some(press);
        dreams
    } else if list(&data["dreams"]).is_empty() {
        card(
            cols,
            &DREAMS,
            "Dreams are on",
            "none yet",
            vec![item(
                "The first look comes after Claude has done some work, and only when CSR has \
                something new about it. \"/csr dreams\" shows the pace.",
            )
            .color(QUIET)],
        )
    } else {
        let items = list(&data["dreams"])
            .iter()
            .take(6)
            .flat_map(|entry| {
                let answer = entry["answer"].as_str().unwrap_or("open");
                let when = stamp(entry["at"].as_f64().unwrap_or(0.), "%m-%d %H:%M");
                [
                    item(format!("{when} · {answer}")).color(FAINT).cut(),
                    item(str_of(&entry["line"])),
                ]
            })
            .collect();
        card(
            cols,
            &DREAMS,
            "Dreams",
            &format!("{} this week", shown(&data["dreams_7d"])),
            items,
        )
    };

    let changes = list(&data["changes"]);
    let changed = (!changes.is_empty()).then(|| {
        let items = changes
            .iter()
            .flat_map(|change| {
                let latest = list(&change["shifts"]).first();
                let mut rows = vec![item(format!(
                    "● {} {}",
                    from(str_of(&change["talked"]), 5),
                    str_of(&change["title"])
                ))
                .bold()
                .cut()];
                if let Some(latest) = latest {
                    let names: Vec<String> = list(&change["shifts"])
                        .iter()
                        .filter(|s| s["oid"] == latest["oid"])
                        .map(|s| {
                            format!(
                                "{}{}",
                                mark_of(str_of(&s["verb"])),
                                symbol_or(&s["symbol"], "(file)")
                            )
                        })
                        .collect();
                    rows.push(
                        item(format!(
                            "  {} {} {}",
                            from(str_of(&latest["date"]), 5),
                            upto(str_of(&latest["oid"]), 7),
                            names.join(" ")
                        ))
                        .color(QUIET)
                        .cut(),
                    );
                }
                rows
            })
            .collect();
        card(
            cols,
            &CHANGED,
            "Changed since your sessions",
            &shown(&data["changes_total"]),
            items,
        )
    });
    let shifts = list(&data["shifts"]);
    let log = (!shifts.is_empty()).then(|| {
        let items = by_commit(shifts)
            .into_iter()
            .flat_map(|(title, group)| {
                let mut rows = vec![item(title).bold().cut()];
                rows.extend(group.into_iter().map(|shift| {
                    let file: Vec<&str> = str_of(&shift["file"]).split('/').collect();
                    let file = file[file.len().saturating_sub(2)..].join("/");
                    item(format!(
                        "  {}{}  {file}",
                        mark_of(str_of(&shift["verb"])),
                        symbol_or(&shift["symbol"], "(whole file)")
                    ))
                    .color(QUIET)
                    .cut()
                }));
                rows
            })
            .collect();
        card(cols, &CHANGED, "Change log", &shown(&data["total"]), items)
    });

    // What CSR can do here: the things this folder has not shown you yet, each marked in its
    // section's ink.
    let mut can = vec![
        item(
            "Ask about past work: \"what did we decide about the pane?\" CSR hands Claude the \
            session.",
        )
        .mark("↑", RECALL),
        item(
            "Ask \"why is this code here?\" Claude traces it to the sessions and commits \
            behind it.",
        )
        .mark("§", CTX),
    ];
    if changes.is_empty() {
        can.push(
            item(if data["git"] == false {
                "Not in git, so code changes cannot show here. Recall still works."
            } else {
                "Code you worked on that a later commit changes shows here, with the commit."
            })
            .mark("~", QUIET),
        );
    }
    can.push(
        item("In herdr, this pane opens as a split beside Claude, with the whole session.")
            .mark("‹›", CTX),
    );
    let can = card(cols, &CAN, "What CSR can do here", "", can);

    // The infinite session: what CSR plans for this session's context, and what its compactions
    // did.
    let plan = lasting
        .and_then(|l| l["plan"].as_str())
        .filter(|t| !t.is_empty());
    let story = lasting
        .and_then(|l| l["story"].as_str())
        .filter(|t| !t.is_empty());
    let session = (plan.is_some() || story.is_some()).then(|| {
        let mut items = Vec::new();
        if let Some(plan) = plan {
            items.push(item(plan).bold());
        }
        if let Some(story) = story {
            items.push(item(story).color(QUIET));
        }
        card(cols, &INFINITE, "Infinite session", "", items)
    });
    // The Shift Map in the pane's own words (the text view, no picture).
    let rows = list(&data["shift_map"]["rows"]);
    let map = (!rows.is_empty()).then(|| {
        let items = rows
            .iter()
            .map(|row| {
                let text = str_of(row);
                if text.starts_with("  ") {
                    item(text).color(QUIET).cut()
                } else {
                    item(text).bold().cut()
                }
            })
            .collect();
        card(
            cols,
            &INFINITE,
            str_of(&data["shift_map"]["title"]),
            "",
            items,
        )
    });

    let mut all = vec![counts];
    all.extend(session);
    all.extend(map);
    all.push(dreams);
    all.extend(offer);
    all.extend(changed);
    all.extend(log);
    all.push(can);
    column(false, all)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_wrap_to_the_width_and_a_long_word_is_cut() {
        assert_eq!(wrapped("", 10), [""]);
        assert_eq!(wrapped("aaa bbb ccc", 7), ["aaa bbb", "ccc"]);
        assert_eq!(wrapped("abcdefghij kl", 4), ["abcd", "kl"]);
        assert_eq!(wrapped("   ", 4), [""]);
    }

    #[test]
    fn a_row_is_filled_to_the_width_with_a_block_in_its_own_ground() {
        let row = paint(10, "#111", vec![part("ab").color("#222").bold()]);
        let last = row["children"].as_array().unwrap().last().unwrap();
        assert_eq!(last["children"][0], "       █");
        assert_eq!(row["children"][0]["props"]["bold"], true);
        assert_eq!(row["children"][0]["props"]["color"], "#222");
    }
}
