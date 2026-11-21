//! "Same page?": the one card that says how Claude and the person are getting on, and the message
//! that puts Claude back on course.
//!
//! The dreamer's fork reads the session and writes `same_page` beside its other observations: how
//! many asks it could see, how many Claude missed, how many the person left unclear, one editorial
//! sentence, and the misses by turn. The numbers are the model's reading, so the card and its popup
//! say so, and a reading whose numbers do not add up is dropped.
//!
//! A session with no reading keeps the plain "What confused the agent" list: that list is not a
//! score, so nothing here turns it into one.

use super::render::observation::dismiss_key;
use super::{array, num, string};
use serde_json::{json, Value};

pub const TITLE: &str = "Same page?";
/// The label of the dreamer's section, and the key of a look's record.
pub const KEY: &str = "same_page";
/// The rows the card's words take, room allowing: the numbers come first, so one row still says them.
pub const ROWS: usize = 3;
const MOST_ASKS: u64 = 1_000_000;
const MOST_MISSES: usize = 5;
const MOST_INTENTS: usize = 5;
const SENTENCE_MAX: usize = 200;
const WORDS_MAX: usize = 160;

/// How the sprite looks, from the share of asks Claude missed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mood {
    Fine,
    Unsure,
    Lost,
}

pub fn mood_of(item: &Value) -> Mood {
    match string(&item["mood"]) {
        "lost" => Mood::Lost,
        "unsure" => Mood::Unsure,
        _ => Mood::Fine,
    }
}

fn mood_word(missed: u64, asks: u64) -> &'static str {
    if asks == 0 || missed * 100 >= asks * 30 {
        if missed == 0 {
            "fine"
        } else {
            "lost"
        }
    } else if missed * 100 >= asks * 10 {
        "unsure"
    } else {
        "fine"
    }
}

fn plain(value: &Value, most: usize) -> Option<String> {
    let text = value.as_str()?.split_whitespace().collect::<Vec<_>>();
    let text = text.join(" ");
    (!text.is_empty() && text.chars().count() <= most).then_some(text)
}

/// The dreamer's `same_page:` line, checked. A broken or impossible reading is no reading.
pub fn parse(reply: &str) -> Value {
    let Some(raw) = reply
        .lines()
        .find_map(|line| line.trim().strip_prefix("same_page:"))
    else {
        return Value::Null;
    };
    let Ok(read) = serde_json::from_str::<Value>(raw.trim()) else {
        return Value::Null;
    };
    let (Some(asks), Some(missed), Some(unclear), Some(line)) = (
        read["asks"].as_u64(),
        read["missed"].as_u64(),
        read["unclear"].as_u64(),
        plain(&read["line"], WORDS_MAX),
    ) else {
        return Value::Null;
    };
    // No session has this many asks: a number past it is noise, and the sums stay in range.
    if asks == 0 || asks > MOST_ASKS || missed > asks || unclear > asks - missed {
        return Value::Null;
    }
    let misses: Vec<Value> = array(&read["misses"])
        .iter()
        .filter_map(|m| {
            Some(json!({
                "turn": m["turn"].as_u64()?,
                "ask": plain(&m["ask"], WORDS_MAX)?,
                "miss": plain(&m["miss"], WORDS_MAX)?,
            }))
        })
        .take(MOST_MISSES)
        .collect();
    // A count with no turn behind it is a claim nobody can check.
    if missed > 0 && misses.is_empty() {
        return Value::Null;
    }
    let intents: Vec<Value> = array(&read["intents"])
        .iter()
        .filter_map(|i| plain(i, WORDS_MAX))
        .take(MOST_INTENTS)
        .map(Value::String)
        .collect();
    json!({
        "asks": asks,
        "missed": missed,
        "unclear": unclear,
        "line": line,
        "misses": misses,
        "intents": intents,
        "next": plain(&read["next"], SENTENCE_MAX),
    })
}

/// Every reading of this session, oldest first.
fn readings<'a>(data: &'a Value, session: &Value) -> Vec<&'a Value> {
    let mut all: Vec<&Value> = array(&data["dreams"])
        .iter()
        .chain(array(&data["dream_records"]))
        .filter(|r| &r["session"] == session && r[KEY].is_object())
        .collect();
    all.sort_by(|a, b| num(&a["at"]).total_cmp(&num(&b["at"])));
    all
}

/// The words a reading's card shows: the key its Hide is stored under.
fn card_line(read: &Value) -> String {
    format!(
        "{READ}{} {}",
        numbers(
            read["asks"].as_u64().unwrap_or(0),
            read["missed"].as_u64().unwrap_or(0),
            read["unclear"].as_u64().unwrap_or(0)
        ),
        string(&read["line"])
    )
}

/// Whose count the card shows. Repeat readings of one session differ, so the front says it is a
/// reading and gives no percentage: 2 of 21 came back as 0 on one read of five.
const READ: &str = "Model's read: ";

fn numbers(asks: u64, missed: u64, unclear: u64) -> String {
    let you = match unclear {
        0 => "You were clear.".to_owned(),
        n => format!("You were unclear on {n}."),
    };
    format!("{missed} of {asks} asks missed. {you}")
}

/// The message `s` types into Claude's prompt. It opens as every CSR note does, so the importer
/// knows the words are CSR's and not the person's.
fn intervention(
    session: &str,
    headline: &str,
    intents: &[String],
    misses: &[String],
    next: &str,
) -> String {
    let mut body = vec![format!("CSR's read of this session: {headline}")];
    if !intents.is_empty() {
        body.push("What the person wants:".into());
        body.extend(intents.iter().map(|i| format!("- {i}")));
    }
    if !misses.is_empty() {
        body.push("Where you missed:".into());
        body.extend(misses.iter().map(|m| format!("- {m}")));
    }
    if !next.is_empty() {
        body.push(format!("How to go on: {next}"));
    }
    let quoted = body
        .iter()
        .map(|l| format!("> {l}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Here is a note offered by CSR (claude-self-reflect):\n\n{quoted}\n\n\
         Stop before your next step. Read csr_transcript(session=\"{session}\", view=\"yourasks\"). \
         Then reply with what the person wants in one line, each miss in your own words, and your \
         next step. Change nothing until they confirm."
    )
}

/// The card's one item: the shape an observation has, so the card opens, folds and hides as one.
/// `found` is what the dreamer flagged as confusing, newest first: the list while no reading exists.
pub fn card(data: &Value, s: &Value, found: Vec<Value>) -> Vec<Value> {
    let session = string(&s["id"]);
    let all = readings(data, &s["id"]);
    // Hide lasts for the session: a later reading words the card anew and stays hidden.
    if all.iter().any(|r| {
        let key = dismiss_key(session, "confusing_claude", &card_line(&r[KEY]));
        data["dismissed"][key] == true
    }) {
        return vec![];
    }
    if let Some(read) = all.last().map(|r| &r[KEY]) {
        let (asks, missed, unclear) = (
            read["asks"].as_u64().unwrap_or(0),
            read["missed"].as_u64().unwrap_or(0),
            read["unclear"].as_u64().unwrap_or(0),
        );
        let headline = numbers(asks, missed, unclear);
        let misses: Vec<String> = array(&read["misses"])
            .iter()
            .map(|m| {
                format!(
                    "Turn {}: asked \"{}\". {}",
                    num(&m["turn"]),
                    string(&m["ask"]),
                    string(&m["miss"])
                )
            })
            .collect();
        let intents: Vec<String> = array(&read["intents"])
            .iter()
            .map(|i| string(i).to_owned())
            .collect();
        let next = string(&read["next"]);
        let turns: Vec<String> = array(&read["misses"])
            .iter()
            .map(|m| num(&m["turn"]).to_string())
            .collect();
        return vec![json!({
            "same_page": true,
            "mood": mood_word(missed, asks),
            "numbers": format!("{READ}{headline}"),
            "words": read["line"],
            "line": card_line(read),
            "evidence": if turns.is_empty() {
                "Read by the dreamer: no misses to cite.".to_owned()
            } else {
                format!("Turns {}", turns.join(", "))
            },
            "explain": if misses.is_empty() {
                "The dreamer read the session and found no miss. It is a model's opinion.".to_owned()
            } else {
                misses.join("\n")
            },
            "next": if next.is_empty() {
                "Press s to type a recap message into Claude's prompt.".to_owned()
            } else {
                next.to_owned()
            },
            "send": intervention(session, &headline, &intents, &misses, next),
        })];
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(body: &str) -> String {
        format!("dream: none\nconfusing_claude: []\nshould_be: []\nsame_page: {body}")
    }

    const GOOD: &str = r#"{"asks":14,"missed":3,"unclear":2,"line":"You ask in one breath and Claude takes the first half.","intents":["Ship the card"],"misses":[{"turn":12,"ask":"open the tab","miss":"It fixed the title instead."}],"next":"Name the done state first."}"#;

    #[test]
    fn a_reading_reads_as_numbers_first_and_the_message_recaps_intents_misses_and_next() {
        let data =
            json!({"dream_records":[{"session":"o","at":1.,"same_page":parse(&reply(GOOD))}]});
        let items = card(&data, &json!({"id":"o"}), vec![]);
        assert_eq!(items.len(), 1);
        let line = string(&items[0]["line"]);
        assert!(
            line.starts_with("Model's read: 3 of 14 asks missed. You were unclear on 2."),
            "{line}"
        );
        let none = r#"{"asks":21,"missed":0,"unclear":1,"line":"Steady.","misses":[]}"#;
        let data =
            json!({"dream_records":[{"session":"o","at":1.,"same_page":parse(&reply(none))}]});
        let shown = card(&data, &json!({"id":"o"}), vec![]);
        let line = string(&shown[0]["line"]);
        assert!(
            line.starts_with("Model's read: 0 of 21 asks missed. You were unclear on 1."),
            "{line}"
        );
        assert_eq!(mood_of(&items[0]), Mood::Unsure);
        let send = string(&items[0]["send"]);
        for part in [
            "CSR's read of this session: 3 of 14 asks missed.",
            "> - Ship the card",
            "> - Turn 12: asked \"open the tab\". It fixed the title instead.",
            "> How to go on: Name the done state first.",
            "csr_transcript(session=\"o\", view=\"yourasks\")",
            "Change nothing until they confirm.",
        ] {
            assert!(send.contains(part), "{part} not in {send}");
        }
    }

    #[test]
    fn numbers_that_do_not_add_up_or_a_miss_with_no_turn_are_no_reading() {
        for bad in [
            "nope",
            r#"{"asks":0,"missed":0,"unclear":0,"line":"x"}"#,
            r#"{"asks":2,"missed":2,"unclear":1,"line":"x","misses":[{"turn":1,"ask":"a","miss":"b"}]}"#,
            r#"{"asks":5,"missed":1,"unclear":0,"line":"no turn cited"}"#,
            r#"{"asks":5,"missed":0,"unclear":0}"#,
            r#"{"asks":18446744073709551615,"missed":18446744073709551615,"unclear":1,"line":"x","misses":[{"turn":1,"ask":"a","miss":"b"}]}"#,
        ] {
            assert!(parse(&reply(bad)).is_null(), "{bad}");
        }
        assert!(parse("dream: none").is_null());
        assert!(!parse(&reply(GOOD)).is_null());
    }

    #[test]
    fn hide_lasts_when_a_later_reading_words_the_card_anew() {
        let first = parse(&reply(GOOD));
        let line = card_line(&first);
        let later = parse(&reply(&GOOD.replace("\"asks\":14", "\"asks\":15")));
        let mut data = json!({"dream_records":[
            {"session":"o","at":1.,"same_page":first},
            {"session":"o","at":2.,"same_page":later},
        ]});
        assert_eq!(card(&data, &json!({"id":"o"}), vec![]).len(), 1);
        data["dismissed"][dismiss_key("o", "confusing_claude", &line)] = json!(true);
        assert!(card(&data, &json!({"id":"o"}), vec![]).is_empty());
    }

    #[test]
    fn no_reading_and_no_finding_is_no_card() {
        assert!(card(&json!({}), &json!({"id":"o"}), vec![]).is_empty());
        let plain = vec![json!({"line":"Misread.","evidence":"Turn 4"})];
        assert_eq!(card(&json!({}), &json!({"id":"o"}), plain.clone()), plain);
    }

    #[test]
    fn the_sprite_follows_the_share_missed() {
        assert_eq!(mood_word(0, 10), "fine");
        assert_eq!(mood_word(1, 20), "fine");
        assert_eq!(mood_word(2, 10), "unsure");
        assert_eq!(mood_word(3, 10), "lost");
    }
}
