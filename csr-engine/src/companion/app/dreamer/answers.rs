//! What became of a dream: the answers kept across sessions (`answered`), a dream answered in
//! the pane (`settledInThePane`), one that reached Claude as a prompt (`sentToClaude`) and one the
//! person said no to (`reject`).
//!
//! The dream log is the pane's file: a dream's answer buttons write it, and these read it only
//! while a dream is up (`settled_in_the_pane`) or a prompt may carry one (`sent_to_claude`).

use super::{
    count, int,
    look::Tenure,
    machine, out,
    text::{dream_sent, own_prompt, rebuts, words_beside, Logged},
};
use crate::companion::app::{
    pace::change_chapter,
    state::{self, KEPT},
};
use crate::companion::relay::dispatch::Ctx;
use anyhow::Result;
use serde_json::{json, Value};

/// The answers that take a dream up: it gives its place in the chapter back (`ADOPTED` in
/// the TypeScript companion, which leaves `sent` to `sent_to_claude`).
pub const ADOPTED: [&str; 2] = ["helpful", "ask_claude"];
/// A prompt is a reply to the dream sent before it only within this long.
pub const REPLY_MS: f64 = 30. * 60. * 1000.;
/// The most looks skipped after dreams typed past.
pub const SKIP_MAX: f64 = 16.;

/// A dream as the dream log holds it, for `dream_sent`.
#[derive(Clone, Debug, PartialEq)]
pub struct LoggedDream {
    pub id: String,
    pub line: String,
    /// Seconds, as the log keeps them.
    pub at: f64,
}

impl Logged for LoggedDream {
    fn line(&self) -> &str {
        &self.line
    }
    fn at(&self) -> f64 {
        self.at
    }
}

/// What the dream log holds of dreams and their answers, in the order written.
#[derive(Debug, Default)]
pub struct LogRead {
    pub dreams: Vec<LoggedDream>,
    /// Each dream's answers, by id, in the order written.
    pub answers: Vec<(String, Vec<String>)>,
}

impl LogRead {
    pub fn read(log: &str) -> Self {
        let mut read = Self::default();
        for line in log.split('\n') {
            let Ok(record) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let id = record["id"].as_str().filter(|id| !id.is_empty());
            let text = |name: &str| record[name].as_str().filter(|text| !text.is_empty());
            if let (Some("dream"), Some(id), Some(said)) =
                (record["kind"].as_str(), id, text("line"))
            {
                let at = record["at"].as_f64().unwrap_or(0.);
                let (id, line) = (id.to_string(), said.to_string());
                read.dreams.push(LoggedDream { id, line, at });
            }
            if let (Some("answer"), Some(id), Some(answer)) =
                (record["kind"].as_str(), id, text("answer"))
            {
                match read.answers.iter_mut().find(|(known, _)| known == id) {
                    Some((_, list)) => list.push(answer.to_string()),
                    None => read
                        .answers
                        .push((id.to_string(), vec![answer.to_string()])),
                }
            }
        }
        read
    }

    pub fn answers_of(&self, id: &str) -> Vec<String> {
        (self.answers.iter())
            .find(|(known, _)| known == id)
            .map(|(_, list)| list.clone())
            .unwrap_or_default()
    }
}

fn zero(cx: &Ctx, key: &str) -> Result<()> {
    cx.store.set(&cx.sid, key, &json!(0))
}

/// What each note came to, kept across sessions: the "at least 1 in 3 acted on" gate reads this.
/// A dream's answer also goes to the dream log, where the pane draws it (`logged`: the pane wrote
/// it there already). `owns`: a look that no longer owns the lease writes nothing.
pub async fn answered(
    cx: &Ctx,
    note: &Value,
    answer: &str,
    logged: bool,
    owns: Option<&Tenure>,
) -> Result<()> {
    let at = cx.clock.now();
    if let Some(tenure) = owns {
        if !tenure.owns().await {
            return Ok(());
        }
    }
    let (id, answer_word) = (note["id"].clone(), answer.to_string());
    state::update(&machine(cx), "answers", |current| {
        let mut list = match current {
            Some(Value::Array(list)) => list,
            _ => Vec::new(),
        };
        list.push(json!({ "id": id, "answer": answer_word, "at": int(at) }));
        let from = list.len().saturating_sub(KEPT);
        Some(Value::Array(list[from..].to_vec()))
    })?;
    if note["kind"] != "dream" || answer == "shown" {
        return Ok(());
    }
    if answer != "ignored" {
        // You answered: the dreamer stops backing off.
        zero(cx, "typedPastInARow")?;
        zero(cx, "looksToSkip")?;
    }
    if ADOPTED.contains(&answer) {
        // You adopted it: it gives its place in the chapter back, if it came in this chapter.
        let came = note["at"].as_f64().unwrap_or(0.);
        let give = |chapter: crate::companion::app::pace::Chapter| {
            if came >= chapter.started {
                crate::companion::app::pace::Chapter {
                    adopted: chapter.adopted + 1.,
                    ..chapter
                }
            } else {
                chapter
            }
        };
        change_chapter(cx, give, None).await?;
    }
    if logged {
        return Ok(()); // the pane wrote it to the dream log already
    }
    let record = json!({ "kind": "answer", "id": note["id"], "answer": answer,
        "at": int(at / 1000.) });
    (out(cx).keep())(cx.cwd.clone(), record).await;
    Ok(())
}

/// A prompt typed past a dream: it was ignored, and the dreamer asks less often, 1, 2, 4... up to
/// 16 looks skipped, as You should know does. The band's lifecycle calls this when the dream it
/// holds has been typed past.
pub async fn typed_past(cx: &Ctx, note: &Value) -> Result<()> {
    answered(cx, note, "ignored", false, None).await?;
    if note["kind"] == "dream" {
        let in_a_row = count(cx, "typedPastInARow") + 1.;
        cx.store
            .set(&cx.sid, "typedPastInARow", &json!(int(in_a_row)))?;
        let skip = SKIP_MAX.min(2f64.powf(in_a_row - 1.));
        cx.store.set(&cx.sid, "looksToSkip", &json!(int(skip)))?;
    }
    Ok(())
}

/// A dream answered in the pane (its answer buttons write the dream log) leaves the band here
/// too, and paces the dreamer as an answer in the band would. The log is read only while a dream
/// is up.
pub async fn settled_in_the_pane(cx: &Ctx) -> Result<()> {
    let up = match cx.store.get(&cx.sid, "view")? {
        Some(view) if view["kind"] != "asked" && view["note"]["kind"] == "dream" => {
            view["note"].clone()
        }
        _ => return Ok(()),
    };
    let Some(log) = out(cx).dream_log().await else {
        return Ok(());
    };
    let mut answer: Option<String> = None;
    for line in log.split('\n') {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if record["kind"] == "answer" && record["id"] == up["id"] && record["by"] == "pane" {
            if let Some(said) = record["answer"].as_str() {
                answer = Some(said.to_string());
            }
        }
    }
    let Some(answer) = answer.filter(|answer| !answer.is_empty()) else {
        return Ok(());
    };
    state::update(cx, "view", |current| match current {
        Some(view) if view["note"]["id"] == up["id"] => None,
        other => other,
    })?;
    answered(cx, &up, &answer, true, None).await
}

/// A dream that reaches Claude as a prompt is logged "sent": the adoption that counts.
/// "ask_claude" only says the box was filled, and a dream sent from the pane was logged "ignored"
/// by the band. Logged once per dream; the first sending of one not yet taken up gives its
/// chapter place back and stops the back-off, as an answer in the band would. A dream you sent on
/// and said no to is "rejected", never sent: your words beside it (`words_beside`), or your next
/// prompt within `REPLY_MS`. Your words are kept with it, and the next brief quotes them to the
/// dreamer.
pub async fn sent_to_claude(cx: &Ctx, text: &str, yours: bool) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    let Some(log) = out(cx).dream_log().await else {
        return Ok(());
    };
    let read = LogRead::read(&log);
    let sent_key = "sentDream";
    let Some(dream) = dream_sent(text, &read.dreams) else {
        // Your next prompt after a dream you sent on: a reply to it, which rejects it if it says
        // no. Only the next.
        let own = if yours { own_prompt(text) } else { None };
        let last = match &own {
            Some(_) => cx.store.get(&cx.sid, sent_key).ok().flatten(),
            None => None,
        };
        let (Some(own), Some(last)) = (own, last) else {
            return Ok(());
        };
        let Some(id) = last["id"].as_str().filter(|id| !id.is_empty()) else {
            return Ok(());
        };
        cx.store.delete(&cx.sid, sent_key)?;
        let sent = read.dreams.iter().find(|candidate| candidate.id == id);
        let within = cx.clock.now() - last["at"].as_f64().unwrap_or(f64::NEG_INFINITY) <= REPLY_MS;
        if let Some(sent) = sent.filter(|_| within && rebuts(&own)) {
            reject(cx, sent, &own, "reply", &read.answers_of(&sent.id)).await?;
        }
        return Ok(());
    };
    let was = read.answers_of(&dream.id);
    if was.iter().any(|answer| answer == "rejected") {
        return Ok(());
    }
    let said = if yours {
        words_beside(text)
    } else {
        String::new()
    };
    if !said.is_empty() && rebuts(&said) {
        let _ = cx.store.delete(&cx.sid, sent_key);
        return reject(cx, dream, &said, "prompt", &was).await;
    }
    if was.iter().any(|answer| answer == "sent") {
        return Ok(());
    }
    let at = cx.clock.now();
    let record = json!({ "kind": "answer", "id": dream.id, "answer": "sent", "by": "prompt",
        "at": int(at / 1000.) });
    (out(cx).keep())(cx.cwd.clone(), record).await;
    zero(cx, "typedPastInARow")?;
    zero(cx, "looksToSkip")?;
    if !was.iter().any(|answer| ADOPTED.contains(&answer.as_str())) {
        let came = dream.at * 1000.;
        let give = |chapter: crate::companion::app::pace::Chapter| {
            if came >= chapter.started {
                crate::companion::app::pace::Chapter {
                    adopted: chapter.adopted + 1.,
                    ..chapter
                }
            } else {
                chapter
            }
        };
        change_chapter(cx, give, None).await?;
    }
    if yours {
        cx.store
            .set(&cx.sid, sent_key, &json!({ "id": dream.id, "at": int(at) }))?;
    }
    Ok(())
}

/// A dream you said no to: "rejected" with your words. An answer, so the back-off stops; a place
/// in the chapter it gave back when it was sent or taken up is the chapter's again.
async fn reject(cx: &Ctx, dream: &LoggedDream, said: &str, by: &str, was: &[String]) -> Result<()> {
    let at = cx.clock.now();
    let record = json!({ "kind": "answer", "id": dream.id, "answer": "rejected", "by": by,
        "said": said, "at": int(at / 1000.) });
    (out(cx).keep())(cx.cwd.clone(), record).await;
    zero(cx, "typedPastInARow")?;
    zero(cx, "looksToSkip")?;
    let taken = |answer: &String| answer == "sent" || ADOPTED.contains(&answer.as_str());
    if was.iter().any(taken) {
        let came = dream.at * 1000.;
        let take_back = |chapter: crate::companion::app::pace::Chapter| {
            if came >= chapter.started {
                crate::companion::app::pace::Chapter {
                    adopted: (chapter.adopted - 1.).max(0.),
                    ..chapter
                }
            } else {
                chapter
            }
        };
        change_chapter(cx, take_back, None).await?;
    }
    Ok(())
}
