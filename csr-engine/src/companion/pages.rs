use super::render::pictures::{Picture, Wanted};
use super::{array, db::Db, dreams, masthead, num, render::text::*, send, string};
/// The masthead's first rows: a click anywhere on them opens the i page.
const MASTHEAD_CLICK: usize = 2;
const PIC: &str = "\x1b[8m\x1b[28m";
use chrono::{Local, TimeZone};
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Default, Clone)]
pub struct Lines {
    pub rows: Vec<String>,
    pub acts: BTreeMap<usize, Value>,
    pub heads: Vec<(usize, String)>,
    pub pictures: Vec<Wanted>,
}
impl Lines {
    fn put(&mut self, rows: Vec<String>, act: Option<&Value>) {
        // a row with no act of its own (a thread's "now in Claude's prompt" line) is a null one
        if let Some(act) = act.filter(|act| !act.is_null()) {
            for i in 0..rows.len() {
                self.acts.insert(self.rows.len() + i, act.clone());
            }
        }
        self.rows.extend(rows);
    }
}
pub fn stamp(at: f64, now: f64) -> String {
    if now - at < 120. {
        return "now".into();
    }
    let Some(when) = Local.timestamp_opt(at as i64, 0).single() else {
        return String::new();
    };
    let today = Local
        .timestamp_opt(now as i64, 0)
        .single()
        .map(|v| v.date_naive());
    when.format(if Some(when.date_naive()) == today {
        "%H:%M"
    } else {
        "%m-%d"
    })
    .to_string()
}
pub use super::recall::{session_story, where_it_stands};
pub(super) fn story_rights(story: &Value, now: f64) -> Vec<String> {
    if story.is_null() {
        return vec![String::new()];
    }
    let mut rev = format!("rev {}", story["rev"]);
    if let Some(id) = story["from_sid"].as_str() {
        rev.push_str(&format!(
            " · from {}",
            id.chars().take(8).collect::<String>()
        ));
    }
    let minutes = ((now - num(&story["at"])).max(0.) / 60.).floor() as u64;
    let old = if minutes < 1 {
        "just told".into()
    } else if minutes < 120 {
        format!("{minutes}m old")
    } else if minutes < 2880 {
        format!("{}h old", minutes / 60)
    } else {
        format!("{}d old", minutes / 1440)
    };
    let by = string(&story["by"]);
    [
        vec![rev.as_str(), by, &old, "next at compact"],
        vec![rev.as_str(), &old, "next at compact"],
        vec![rev.as_str(), &old],
        vec![&old],
    ]
    .iter()
    .map(|p| {
        p.iter()
            .copied()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" · ")
    })
    .collect()
}
fn section_value(
    name: &str,
    title: &str,
    right: &str,
    items: Vec<Value>,
    at: Value,
    brief: &str,
) -> Value {
    json!({
        "name": name,
        "title": title,
        "right": right,
        "items": items,
        "at": at,
        "brief": brief,
    })
}
pub fn page_main(data: &Value, s: &Value, now: f64, ui: &Value, db: Option<&Db>) -> Value {
    let sid = string(&s["id"]);
    // on a phone the pane is the whole screen and tall: capped sections show more, and more of the story
    let more = if ui["phone"] == true {
        super::lostmain::PHONE_MORE
    } else {
        1
    };
    let (rules, rules_at) = db
        .map(|db| super::recall::rules(db, s, string(&data["repo"]), 3 * more))
        .unwrap_or_default();
    let fed = dreams::feed(array(&data["dreams"]), &data["chapter"], sid);
    let star = &fed["star"];
    let starred = !star.is_null();
    let mut items = vec![];
    let dream_send = |d: &Value| {
        let reasons: Vec<_> = string(&d["text"])
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.trim_start_matches(['*', '-', ' ']))
            .collect();
        send::closed_send(
            &format!(
                "CSR's dreamer asked this while you worked, quoted: “{}” Its reasoning:",
                send::straight(string(&d["line"]))
            ),
            &format!(
                "{}. {}",
                string(&d["title"]),
                reasons
                    .iter()
                    .take(3)
                    .copied()
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
            700,
        )
    };
    if starred {
        let reasons: Vec<_> = string(&star["text"])
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.trim_start_matches(['*', '-', ' ']))
            .collect();
        let mut body = vec![string(&star["title"])];
        body.extend(reasons.iter().take(1).copied());
        items.push(json!({
            "key": format!("dream:{}", string(&star["id"])),
            "dream": star["id"],
            "title": star["line"],
            "lines": 3,
            "body": body,
            "answers": true,
            "detail": reasons.iter().skip(1).take(2).collect::<Vec<_>>(),
            "send": dream_send(star),
        }));
    }
    let mut earlier = array(&fed["adopted"]).to_vec();
    earlier.extend_from_slice(array(&fed["folded"]));
    earlier.sort_by(|a, b| num(&b["at"]).total_cmp(&num(&a["at"])));
    let passed = array(&fed["passed"]).len();
    let took = earlier.len() - passed;
    let folded = [
        if took > 0 {
            format!("{took} adopted")
        } else {
            String::new()
        },
        if passed > 0 {
            format!("{passed} passed")
        } else {
            String::new()
        },
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join(" · ");
    if !earlier.is_empty() {
        let open = array(&ui["folds"]).contains(&json!("passed"));
        items.push(json!({
            "key": "fold:passed",
            "quiet": true,
            "lines": 1,
            "title": format!("{} {folded}", if open { "⌄" } else { "›" }),
            "detail": [],
            "send": "",
        }));
        if open {
            for d in &earlier {
                // pane.py reasons_of: one leading * or - and the spaces after it come off
                let mut detail = vec![json!(string(&d["title"]))];
                detail.extend(
                    string(&d["text"])
                        .lines()
                        .filter(|l| !l.trim().is_empty())
                        .take(2)
                        .map(|l| json!(l.strip_prefix(['*', '-']).map_or(l, str::trim_start))),
                );
                items.push(json!({
                    "key": format!("dream:{}", string(&d["id"])),
                    "title": d["line"],
                    "lines": 1,
                    "unsigned": true,
                    "meta": format!(
                        "{} · {}",
                        stamp(num(&d["at"]), now),
                        dreams::answer_word(string(&d["answer"]))
                    ),
                    "detail": detail,
                    "send": dream_send(d),
                }));
            }
        }
    }
    let right = if data["dreams_on"] == false {
        "off".into()
    } else if fed["left"] == 0 {
        "0 left · next compact".into()
    } else if !data["look"].is_null() && !starred {
        let when = stamp(num(&data["look"]["at"]), now);
        format!(
            "looked {} · {} left",
            if when == "now" { "just now" } else { &when },
            fed["left"]
        )
    } else {
        format!("{} of {} left", fed["left"], fed["of"])
    };
    let story = &data["story"];
    let rights = story_rights(story, now);
    let mut story_items = if story.is_null() {
        vec![]
    } else {
        vec![json!({
            "key": "story",
            "like": format!("story:{sid}:{}", story["rev"]),
            "title": story["now"],
            "open_title": story["text"],
            "lines": 6 * more,
            "open_lines": 30,
            "detail": [],
            "send": send::closed_send(
                &format!("CSR's story of this session, rev {}, quoted:", story["rev"]),
                string(&story["text"]),
                700,
            ),
        })]
    };
    story_items.extend(super::shiftmap::map_items(data, s, ui));
    let dream_at = earlier
        .iter()
        .chain(if starred { Some(star) } else { None })
        .map(|d| num(&d["at"]))
        .fold(0., f64::max);
    let mut story_section = section_value(
        "story",
        "Story",
        &rights[0],
        story_items,
        story["at"].clone(),
        "",
    );
    story_section["rights"] = json!(&rights[1..]);
    let mut dream_section = section_value(
        "dreams",
        "What if…",
        &right,
        items,
        json!(dream_at),
        &folded,
    );
    dream_section["star"] = json!(starred);
    dream_section["waits"] = json!(starred);
    let mut sections = vec![
        story_section,
        dream_section,
        section_value("rules", "Your rules", "", rules, json!(rules_at), ""),
    ];
    if let Some(db) = db {
        if let Some(handed) = remembered_section(db, s, now, 2 * more) {
            sections.insert(2, handed);
        }
    }
    for (k, lost) in super::lostmain::lost_sections(s, now, 3 * more)
        .into_iter()
        .enumerate()
    {
        sections.insert(2 + k, lost);
    }
    // How Claude read you (before Your rules), then In flight (right after the story)
    let readings: Vec<super::coach::Reading> =
        serde_json::from_value(data["readings"].clone()).unwrap_or_default();
    let coach_at = sections.iter().position(|s| s["name"] == "rules");
    sections.insert(
        coach_at.unwrap_or(sections.len()),
        super::readings::coach_section(&readings),
    );
    if let Some(flight) =
        super::flight::flight_section(&data["agents"], &data["codex"], now, array(&ui["folds"]))
    {
        let story_at = sections.iter().position(|s| s["name"] == "story");
        sections.insert(story_at.map_or(0, |at| at + 1), flight);
    }
    let waiting = super::card::lost_waiting(&s["lost"]);
    if let Some(card) =
        super::card::compact_section(&data["infinite"]["card"], &data["compacting"], waiting)
    {
        // CSR's own compaction: under the story, which is pinned on top (the feed keeps it there)
        sections.insert(1, card);
    }
    let g = masthead::gauges_compacting(
        &data["claude_fields"],
        data["claude_at"].as_f64(),
        s,
        now,
        &data["limits"],
        &data["sts"],
        &data["compacting"],
    );
    let mut page = json!({
        "head": {
            "gauges": g,
            // the mod's plan and what CSR's compactions did
            "infinite": data["infinite"],
        },
        "quiet": "Nothing to report yet. A section shows here once CSR has \
            something for it.",
        "sections": sections,
    });
    if default_view(&page, s) {
        let rows = ui["rows"].as_u64().unwrap_or(30) as usize;
        page = page_default(page, data, s, now, rows);
    }
    page
}
pub fn default_view(page: &Value, s: &Value) -> bool {
    if string(&s["id"]).is_empty() {
        return false;
    }
    if num(&s["turn_end"]) == 0. {
        return true;
    }
    if array(&page["sections"]).iter().any(|section| {
        !array(&section["items"]).is_empty()
            && !matches!(string(&section["name"]), "rules" | "lost")
    }) {
        return false;
    }
    array(&s["lost"])
        .iter()
        .filter(|t| {
            !t["hidden"]
                .as_bool()
                .unwrap_or_else(|| t["hidden"].as_str().is_some_and(|s| !s.is_empty()))
        })
        .all(|t| t["state"] == "carried")
}
fn page_default(mut page: Value, data: &Value, s: &Value, now: f64, rows: usize) -> Value {
    let mut sections = vec![];
    let fed = dreams::feed(array(&data["dreams"]), &data["chapter"], string(&s["id"]));
    if !fed["star"].is_null() {
        if let Some(s) = array(&page["sections"])
            .iter()
            .find(|s| s["name"] == "dreams")
        {
            sections.push(s.clone());
        }
    }
    if let Some(told) = told_section(s) {
        sections.push(told);
    }
    if let Some(last) = last_section(s, now, rows) {
        sections.push(last);
    }
    if let Some(left) = super::lostmain::left_over(s, now) {
        sections.push(left);
    }
    let todos: Vec<_> = array(&s["todos"])
        .iter()
        .enumerate()
        .map(|(i, t)| {
            json!({
                "key": format!(
                    "todo:{}:{i}",
                    string(&s["last_session"]["id"])
                        .chars()
                        .take(8)
                        .collect::<String>()
                ),
                "title": t["subject"],
                "lines": 2,
                "meta": format!(
                    "{} · from {}",
                    string(&t["status"]).replace('_', " "),
                    string(&s["last_session"]["id"])
                        .chars()
                        .take(8)
                        .collect::<String>()
                ),
                "send": one_line(
                    &format!(
                        "Left over from session {}: “{}” ({}).",
                        string(&s["last_session"]["id"])
                            .chars()
                            .take(8)
                            .collect::<String>(),
                        string(&t["subject"]),
                        string(&t["status"])
                    ),
                    700,
                ),
            })
        })
        .collect();
    if !todos.is_empty() {
        sections.push(section_value(
            "todos",
            "Todos left",
            "",
            todos,
            s["last_session"]["last"].clone(),
            "",
        ));
    }
    let begun = s["told"]["at"].clone();
    for s in &mut sections {
        s["born"] = begun.clone();
    }
    let taken: Vec<String> = sections
        .iter()
        .map(|s| string(&s["name"]).to_owned())
        .chain(["lost".to_owned(), "story".to_owned()])
        .collect();
    sections.extend(
        array(&page["sections"])
            .iter()
            .filter(|s| {
                !array(&s["items"]).is_empty() && !taken.iter().any(|t| t == string(&s["name"]))
            })
            .cloned(),
    );
    page["sections"] = json!(sections);
    page["quiet"] = json!(if s["last_session"].is_null() {
        "Reading the session before this one…"
    } else {
        "First session here: no earlier session in this project to carry over."
    });
    page
}
fn last_sparkline(facts: &Value, width: usize) -> String {
    let series = array(&facts["series"]);
    if series.len() < 2 || width < 8 {
        return String::new();
    }
    let first = num(&series[0][0]);
    let last = num(&series[series.len() - 1][0]);
    let mut bins = vec![0_f64; width];
    for r in series {
        let i = (((num(&r[0]) - first) / (last - first).max(1.) * width as f64) as usize)
            .min(width - 1);
        bins[i] = bins[i].max(num(&r[1]));
    }
    let peak = bins.iter().copied().fold(0., f64::max).max(1.);
    let mut level = 0.;
    bins.into_iter()
        .map(|b| {
            if b > 0. {
                level = b;
            }
            if level > 0. {
                EIGHTHS[1 + (level / peak * 7.999) as usize]
            } else {
                ' '
            }
        })
        .collect()
}
fn last_section(s: &Value, now: f64, rows: usize) -> Option<Value> {
    let f = &s["last_session"];
    let id = f["id"].as_str()?;
    let id8 = id.chars().take(8).collect::<String>();
    let title = one_line(
        f["title"]
            .as_str()
            .or_else(|| f["typed"][1].as_str())
            .unwrap_or(&format!("session {id8}")),
        200,
    );
    let active = num(&f["active"]);
    let prompts = num(&f["prompts"]) as u64;
    let compacts = num(&f["compacts"]) as u64;
    let first = [
        format!("{} active", masthead::span(active)),
        if prompts > 0 {
            format!("{prompts} prompts")
        } else {
            String::new()
        },
        if compacts > 0 {
            format!("{compacts} compacts")
        } else {
            String::new()
        },
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join(" · ");
    let learned = num(&f["learned"]);
    let handed = num(&f["handed"]);
    let peak = num(&f["peak"]);
    let second = [
        if learned > 0. {
            format!("CSR learned {}", super::pyfmt::short(learned))
        } else {
            String::new()
        },
        if handed > 0. {
            format!(
                "{} {handed}×",
                if learned > 0. { "handed" } else { "CSR handed" }
            )
        } else {
            String::new()
        },
        if peak > 0. {
            format!("peak {}", super::pyfmt::short(peak))
        } else {
            String::new()
        },
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join(" · ");
    let right = if super::truthy(&f["last"]) {
        format!(
            "{id8} · {}",
            if s["last_running"] == true {
                "still running".to_owned()
            } else {
                stamp(num(&f["last"]), now)
            }
        )
    } else {
        id8.clone()
    };
    let mut detail = vec![];
    if num(&f["floor"]) > 0. && compacts > 0 {
        detail.push(format!(
            "After a compaction the context settled near {}, the median of the last {}.",
            super::pyfmt::short(num(&f["floor"])),
            compacts.min(5)
        ));
    }
    let quoted = |text: &str| {
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        format!(
            "“{}”",
            if text.chars().count() <= 200 {
                text
            } else {
                format!("{}…", text.chars().take(199).collect::<String>().trim_end())
            }
        )
    };
    if let Some(text) = f["typed"][1].as_str() {
        detail.push(format!("You last: {}", quoted(text)));
    }
    if let Some(text) = f["said"][1].as_str() {
        detail.push(format!("Claude last: {}", quoted(text)));
    }
    let last_row = super::recall::date(num(&f["last"]), "%m-%d %H:%M");
    let item = json!({
        "key": format!("last:{id}"),
        "title": title,
        "lines": 1,
        "fit_body": true,
        "picture": {
            "facts": f,
            "rows": if rows >= 30 { 4 } else { 3 },
        },
        "body": [first, second],
        "detail": detail,
        "send": one_line(
            &format!(
                "Last session {id8} (“{title}”): {} active, {prompts} prompts, \
                    {compacts} compactions, last row {last_row}. The rest of it: \
                    csr_reflect_on_past(\"conv_{id}\")",
                masthead::span(active)
            ),
            700,
        ),
    });
    Some(section_value(
        "last",
        "Last session",
        &right,
        vec![item],
        f["last"].clone(),
        &id8,
    ))
}
fn told_section(s: &Value) -> Option<Value> {
    let told = &s["told"];
    let text = string(&told["text"]);
    if text.is_empty() {
        return None;
    }
    let lines: Vec<_> = text.lines().map(str::trim).collect();
    let recap = lines
        .iter()
        .find(|l| l.starts_with("recap ["))
        .map(|s| s.trim_end_matches("[[CSR:RECAP]]").trim())
        .or_else(|| {
            lines.iter().copied().find(|l| {
                !l.is_empty()
                    && ![
                        "CSR ENDLESS MEMORY ACTIVE",
                        "The blocks below are memory",
                        "Reflex:",
                        "[briefing",
                        "#",
                        "**",
                    ]
                    .iter()
                    .any(|prefix| l.starts_with(prefix))
            })
        })?;
    let dreamt: Vec<_> = lines
        .iter()
        .filter(|l| l.starts_with("Dreamt:"))
        .copied()
        .collect();
    let anchors = lines
        .iter()
        .find(|l| l.starts_with("ANCHORS:") || l.starts_with("ANCHORS ("))
        .copied()
        .unwrap_or("");
    let full = lines
        .iter()
        .find_map(|l| l.strip_prefix("Full state:"))
        .unwrap_or("")
        .trim();
    let index = lines
        .iter()
        .position(|l| l.contains("EPISODE INDEX"))
        .map(|i| {
            lines[i + 1..]
                .iter()
                .filter(|l| l.starts_with("- ["))
                .count()
        })
        .unwrap_or(0);
    let recap = one_line(recap, 600);
    let full = one_line(full, 300);
    let mut body = dreamt
        .iter()
        .take(1)
        .map(|s| one_line(s, 300))
        .collect::<Vec<_>>();
    let anchor = [
        one_line(anchors, 300),
        if index > 0 {
            format!("{index} earlier threads")
        } else {
            String::new()
        },
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join(" · ");
    if !anchor.is_empty() {
        body.push(anchor);
    }
    let mut detail: Vec<_> = dreamt
        .iter()
        .skip(1)
        .take(2)
        .map(|s| one_line(s, 300))
        .collect();
    if !full.is_empty() {
        detail.push(one_line(&full, 300));
    }
    let source = told["source"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or("startup");
    let said = match source {
        "startup" => "at start".into(),
        "resume" => "at resume".into(),
        "compact" => "after compact".into(),
        "clear" => "after clear".into(),
        _ => one_line(source, 12),
    };
    let when = told["at"]
        .as_f64()
        .and_then(|t| Local.timestamp_opt(t as i64, 0).single())
        .map(|d| d.format("%H:%M").to_string())
        .unwrap_or_default();
    Some(section_value(
        "told",
        "CSR told Claude",
        format!("{said} {when}").trim(),
        vec![json!({
            "key": "told:recap",
            "like": format!("told:{}:{}", string(&s["id"]), told["at"]),
            "title": one_line(&recap, 600),
            "lines": 3,
            "plain_body": true,
            "body": body,
            "detail": detail,
            "send": one_line(
                &format!(
                    "CSR's recap at the start of this session: {recap}{}",
                    if full.is_empty() {
                        String::new()
                    } else {
                        format!(" The rest: {full}")
                    }
                ),
                700,
            ),
        })],
        told["at"].clone(),
        &said,
    ))
}
fn from_when(at: f64, now: f64) -> String {
    if at == 0. {
        return String::new();
    }
    let days = (now - at) / 86400.;
    if days < 1. {
        "earlier today".into()
    } else if days < 2. {
        "yesterday".into()
    } else if days < 14. {
        format!("{} days ago", days as i64)
    } else if days < 60. {
        format!("{} weeks ago", (days / 7.) as i64)
    } else {
        format!("{} months ago", (days / 30.) as i64)
    }
}
fn remembered_section(db: &Db, s: &Value, now: f64, most: usize) -> Option<Value> {
    let recalled = super::recall::remembered(db, s);
    let at = recalled.iter().map(|r| num(&r["at"])).fold(0., f64::max);
    let items: Vec<_> = recalled
        .iter()
        .take(most)
        .map(|r| {
            let id = string(&r["key"]);
            let times = array(&r["seconds"]).len().max(1);
            let when = from_when(num(&r["when"]), now);
            let project = string(&r["project"]);
            let where_ = [when.as_str(), project]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
            let where_ = if times > 1 {
                format!("{where_} · {times}×")
            } else {
                where_
            };
            let title = one_line(&string(&r["title"]).replace("**", ""), 700);
            json!({
                "key": format!("recall:{id}"),
                "title": title,
                "meta": format!("opened · {where_}"),
                "detail": [format!(
                    "Session {} · {} · handed to Claude {times}× this session.",
                    &id[..8],
                    [when.as_str(), project]
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join(" · ")
                )],
                "send": one_line(
                    &format!(
                        "CSR remembered a past session: “{title}” ({}). The rest of it: \
                            csr_reflect_on_past(\"conv_{id}\")",
                        [when.as_str(), project]
                            .into_iter()
                            .filter(|s| !s.is_empty())
                            .collect::<Vec<_>>()
                            .join(" · ")
                    ),
                    700,
                ),
            })
        })
        .collect();
    if items.is_empty() {
        None
    } else {
        Some(section_value(
            "remembered",
            "Handed to Claude",
            "",
            items,
            json!(at),
            &format!("{} opened", recalled.len().min(2)),
        ))
    }
}
pub fn page_log(data: &Value) -> Value {
    let mut groups: Vec<((String, String), Vec<&Value>)> = vec![];
    for s in array(&data["shifts"]) {
        let key = (string(&s["date"]).into(), string(&s["oid"]).into());
        if let Some((_, g)) = groups.iter_mut().find(|(k, _)| *k == key) {
            g.push(s);
        } else {
            groups.push((key, vec![s]));
        }
    }
    let items: Vec<_> = groups
        .into_iter()
        .map(|((date, oid), g)| {
            let mut files: Vec<(&str, Vec<&Value>)> = vec![];
            for s in &g {
                let file = string(&s["file"]);
                if let Some((_, v)) = files.iter_mut().find(|(f, _)| *f == file) {
                    v.push(s);
                } else {
                    files.push((file, vec![s]));
                }
            }
            let short_path = |path: &str| {
                path.split('/')
                    .rev()
                    .take(2)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("/")
            };
            let named = files
                .iter()
                .take(3)
                .map(|(f, changes)| {
                    format!(
                        "{} ({})",
                        short_path(f),
                        changes
                            .iter()
                            .take(4)
                            .map(|s| s["symbol"]
                                .as_str()
                                .filter(|s| !s.is_empty())
                                .unwrap_or("whole file"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            let detail: Vec<_> = files
                .iter()
                .take(4)
                .map(|(f, changes)| {
                    format!(
                        "{}: {}",
                        short_path(f),
                        changes
                            .iter()
                            .take(6)
                            .map(|s| format!(
                                "{}{}",
                                match string(&s["verb"]) {
                                    "changed" => "~",
                                    "removed" => "−",
                                    "came back" => "+",
                                    _ => "",
                                },
                                s["symbol"]
                                    .as_str()
                                    .filter(|s| !s.is_empty())
                                    .unwrap_or("whole file")
                            ))
                            .collect::<Vec<_>>()
                            .join(" ")
                    )
                })
                .collect();
            let id7 = oid.chars().take(7).collect::<String>();
            let subject = string(&g[0]["subject"]);
            json!({
                "key": format!("commit:{oid}"),
                "title": if subject.is_empty() { &id7 } else { subject },
                "meta": format!(
                    "{id7} · {} · {} {}",
                    date.chars().skip(5).collect::<String>(),
                    files.len(),
                    if files.len() == 1 { "file" } else { "files" }
                ),
                "detail": detail,
                "send": one_line(
                    &format!("Since our past sessions, commit {id7} “{subject}” ({date}) \
                        changed code we worked on: {named}."),
                    700,
                ),
            })
        })
        .collect();
    json!({
        "head": {
            "title": "Commits under your past work",
            "facts": "When Claude opens one of these files, CSR gives it a Heads up.",
        },
        "quiet": "Nothing changed under past conversations here.",
        "sections": [{
            "name": "changed",
            "title": "Changed since",
            "right": data["total"].to_string(),
            "items": items,
        }],
    })
}
pub fn heading(s: &Value, width: usize, at: usize) -> String {
    heading_mode(s, width, at, false)
}
fn heading_mode(s: &Value, width: usize, at: usize, pixels: bool) -> String {
    let name = string(&s["name"]);
    let title = string(&s["title"]);
    let tape = s["life"]
        .as_f64()
        .map(|v| format!(" {}", EIGHTHS[fill_levels(v, 1)[0]]))
        .unwrap_or_default();
    let room = width.saturating_sub(at + len(&tape));
    let mut forms = vec![string(&s["right"])];
    forms.extend(array(&s["rights"]).iter().map(string));
    let right = forms
        .into_iter()
        .find(|r| room as isize - len(title) as isize - len(r) as isize > 1)
        .unwrap_or("");
    let gap = room.saturating_sub(len(title) + len(right) + 1).max(1);
    let lead = if at < 6 || pixels {
        " ".repeat(at)
    } else {
        format!(
            " {}{} ",
            glyph(name),
            " ".repeat(4_usize.saturating_sub(len(glyph(name))))
        )
    };
    format!(
        "{lead}{}{BOLD}{}{OFF}{}{}{right}{tape}{OFF}",
        ink(if name == "dreams" {
            section(name)
        } else {
            "quiet"
        }),
        fit(title, room.saturating_sub(1)),
        " ".repeat(gap),
        ink("quiet")
    )
}
pub fn item_rows(
    item: &Value,
    name: &str,
    width: usize,
    is_open: bool,
    at: usize,
    heart: Option<bool>,
    sent: &Value,
) -> Vec<String> {
    item_rows_mode(item, name, width, is_open, at, heart, (sent, false))
}
fn item_rows_mode(
    item: &Value,
    name: &str,
    width: usize,
    is_open: bool,
    at: usize,
    heart: Option<bool>,
    mode: (&Value, bool),
) -> Vec<String> {
    let (sent, pixels) = mode;
    let room = width.saturating_sub(at);
    let flag = string(&item["flag"]);
    let meta = string(&item["meta"]);
    let spare = usize::from(heart.is_some()) * 2;
    let title = if is_open {
        item["open_title"]
            .as_str()
            .unwrap_or(string(&item["title"]))
    } else {
        string(&item["title"])
    }
    .replace('`', "");
    let mut voice = ink(if item["quiet"] == true {
        "quiet"
    } else {
        "you"
    });
    // a figure that nears its limit: ochre, then vermilion
    if let Some(tone) = item["tone"]
        .as_str()
        .filter(|t| matches!(*t, "amber" | "red"))
    {
        voice = format!(
            "{}{BOLD}",
            ink(if tone == "amber" { "dream" } else { "turn" })
        );
    }
    let pad = " ".repeat(at);
    let mut rows: Vec<_> = wrap(
        &title,
        room.saturating_sub(if flag.is_empty() && meta.is_empty() {
            spare
        } else {
            0
        }),
        item[if is_open { "open_lines" } else { "lines" }]
            .as_u64()
            .unwrap_or(if is_open { 6 } else { 2 }) as usize,
    )
    .into_iter()
    .map(|l| format!("{pad}{voice}{l}{OFF}"))
    .collect();
    for line in array(&item["lede"]) {
        rows.push(format!("{pad}{voice}{}{OFF}", fit(string(line), room)));
    }
    let picture = if is_open && item["open_picture"].is_object() {
        &item["open_picture"]
    } else {
        &item["picture"]
    };
    if picture.is_object() && pixels && room >= 24 {
        rows.extend(vec![
            format!("{pad}\x1b[8m\x1b[28m");
            picture["rows"].as_u64().unwrap_or(0).min(1000) as usize
        ]);
    } else if picture.is_object()
        && room >= 8
        && picture["kind"].as_str().unwrap_or("last") == "last"
    {
        rows.push(format!(
            "{pad}{}{}{OFF}",
            ink("ctx"),
            last_sparkline(&picture["facts"], room)
        ));
    }
    for (i, text) in array(&item["body"]).iter().enumerate() {
        let voice = ink(
            if array(&item["body_loud"]).contains(&json!(i))
                || i < item["loud"].as_u64().unwrap_or(0) as usize
            {
                "you"
            } else {
                "quiet"
            },
        );
        if item["fit_body"] == true {
            rows.push(format!(
                "{pad}{voice}{}{OFF}",
                fit_pieces(string(text), room)
            ));
        } else {
            for line in wrap(string(text), room, if i == 0 { 1 } else { 2 }) {
                rows.push(format!(
                    "{pad}{voice}{}{line}{OFF}",
                    if i == 0 && item["plain_body"] != true {
                        BOLD
                    } else {
                        ""
                    }
                ));
            }
        }
    }
    let text = fit(
        &[flag, meta]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" · "),
        room.saturating_sub(spare),
    );
    if !text.is_empty() {
        let line = if !flag.is_empty() && text.starts_with(flag) {
            format!(
                "{}{flag}{}{}",
                ink("turn"),
                ink("faint"),
                &text[flag.len()..]
            )
        } else {
            format!("{}{text}", ink("faint"))
        };
        rows.push(format!("{pad}{line}{OFF}"));
    }
    if let Some(liked) = heart {
        let i = if text.is_empty() { 0 } else { rows.len() - 1 };
        let gap = width.saturating_sub(1 + len(&plain(&rows[i]))).max(1);
        rows[i].push_str(&format!(
            "{}\x1b[24m{}{}{OFF}",
            " ".repeat(gap),
            ink("faint"),
            if liked { "♥" } else { "♡" }
        ));
    }
    if is_open {
        let mut more = vec![];
        for text in array(&item["detail"]) {
            more.extend(
                wrap(
                    string(text),
                    room,
                    if item["whole_detail"] == true { 99 } else { 2 },
                )
                .into_iter()
                .map(|s| format!("{pad}{DIM}{s}{OFF}")),
            );
        }
        if item["whole_detail"] != true {
            more.truncate(4);
        }
        rows.extend(more);
        let hint = if let Some(h) = item["hint"].as_str() {
            h
        } else if sent[0] == item["key"] {
            if sent[1] == true {
                "✓ now in Claude's prompt"
            } else {
                "✗ couldn't reach Claude's pane"
            }
        } else {
            "↳ click again → Claude's prompt"
        };
        rows.push(format!(
            "{pad}{}{}{OFF}",
            ink(section(name)),
            fit(hint, room)
        ));
        let n = rows.len();
        for (i, row) in rows.iter_mut().enumerate() {
            let cut = at.saturating_sub(1);
            let mut s = format!(
                "{}{}▌{OFF}{}",
                row.chars().take(cut).collect::<String>(),
                ink(section(name)),
                row.chars().skip(at).collect::<String>()
            );
            if i < n - 1 {
                s = tinted(&s, &bg(tint(name, 0.15)));
            }
            *row = s;
        }
    }
    rows
}
/// The lines that close a sparse main page: what CSR can do here, leaving out what the page
/// already shows (a "told" card says what CSR handed Claude; a "changed" card lists the
/// changes; a "lost" card lists the threads). None of them draws unless it fits in `room`.
fn closer(page: &Value, width: usize, at: usize, room: usize) -> Vec<String> {
    let shown = |name: &str| {
        array(&page["sections"])
            .iter()
            .any(|s| s["name"] == name && !array(&s["items"]).is_empty())
    };
    let mut lines = vec![];
    if !shown("told") {
        lines.push("Ask about past work. CSR finds it.");
    }
    if !shown("changed") {
        lines.push("Code changed? CSR warns Claude.");
    }
    if !shown("lost") {
        lines.push("CSR keeps quiet threads. Press f.");
    }
    lines.push("Press l for changes. Press i for the figure.");
    let lines: Vec<String> = lines
        .into_iter()
        .flat_map(|l| wrap(l, width.saturating_sub(at), 2))
        .map(|l| format!("{}{}{l}{OFF}", " ".repeat(at), ink("faint")))
        .collect();
    // one blank row, then the lines: all of them or none, with a row to spare
    if lines.len() + 2 <= room {
        std::iter::once(String::new()).chain(lines).collect()
    } else {
        vec![]
    }
}
pub fn draw_page(page: &Value, cols: usize, rows: usize, ui: &Value) -> Lines {
    let mut out = draw_sections(page, cols, rows, ui);
    if page["head"]["gauges"].is_object() {
        let width = cols.saturating_sub(1).max(24);
        let at = if ui["phone"] == true { 2 } else { 6 };
        let room = rows.saturating_sub(out.rows.len());
        out.put(closer(page, width, at, room), None);
    }
    out
}
fn draw_sections(page: &Value, cols: usize, rows: usize, ui: &Value) -> Lines {
    let width = cols.saturating_sub(1).max(24);
    let at = if ui["phone"] == true { 2 } else { 6 };
    let head = &page["head"];
    let pixels = ui["pixels"] == true;
    // the cards' pill takes the two masthead rows when the pane draws cards and it fits
    let pill = (ui["cards"] == true && head["gauges"].is_object())
        .then(|| {
            let fonts = super::render::cards::fonts()?;
            let cell = (
                u32::try_from(ui["cell"][0].as_u64()?).ok()?,
                u32::try_from(ui["cell"][1].as_u64()?).ok()?,
            );
            let pill = super::render::cards::pill_of(
                &head["gauges"],
                ui["csr"].as_u64().unwrap_or(0) as usize,
                ui["dreams"].as_u64().map(|n| n as usize),
                (width - 1) as f32 * cell.0 as f32 / 2. - 8.,
                fonts,
            )?;
            let cols = super::render::cards::pill_cols(&pill, cell, fonts);
            (cols < width).then_some((pill, cols))
        })
        .flatten();
    let logo = pixels && head["gauges"].is_object() && pill.is_none();
    let mut top = if head["gauges"].is_object() {
        let mut top = cockpit(&head["gauges"], width);
        if logo {
            let label = slot("CTX ", "normal");
            top[0] = top[0].replacen(&label, &slot("    ", "normal"), 1);
        }
        top
    } else {
        vec![
            format!(" {BOLD}{}{OFF}", fit(string(&head["title"]), width - 1)),
            format!(" {}", fit_pieces(string(&head["facts"]), width - 1)),
        ]
    };
    let lanes = if !head["gauges"].is_object() && head["session"].is_object() {
        let start = top.len();
        if pixels && width > 17 {
            let label = fg(blend([238, 245, 250], [47, 58, 72], 0.45));
            top.extend(
                ["you", "context", "memory"]
                    .into_iter()
                    .map(|name| format!(" {label}{name:<8}{OFF}")),
            );
        }
        top.push(format!(
            "  {}",
            lanes_legend(&head["session"], width - 2, true)
        ));
        (pixels && width > 17).then_some(start)
    } else {
        None
    };
    if pill.is_some() {
        top[0] = " ".into();
        top[1] = " ".into();
    }
    // the mod's plan for the next compaction, and the story
    top.extend(super::card::infinite_rows(&head["infinite"], width));
    let grounds = dark_steps(top.len());
    let mut out = Lines::default();
    if let Some(row) = lanes {
        out.pictures.push(Wanted {
            row,
            col: 9,
            cols: width - 9,
            rows: 3,
            picture: Picture::Session(super::render::raster::SessionFacts::from_value(
                &head["session"],
                num(&ui["now"]),
            )),
            backs: grounds[row..row + 3].to_vec(),
        });
    }
    if let Some((pill, cols)) = pill {
        out.pictures.push(Wanted {
            row: 0,
            col: 1,
            cols,
            rows: 2,
            picture: Picture::Pill(pill),
            backs: grounds[..2].to_vec(),
        });
    }
    if logo {
        out.pictures.push(Wanted {
            row: 0,
            col: 1,
            cols: 4,
            rows: 1,
            picture: Picture::Logo,
            backs: vec![grounds[0]],
        });
    }
    for (row, (text, ground)) in top.iter().zip(grounds).enumerate() {
        out.rows.push(on_dark(text, ground));
        if row < MASTHEAD_CLICK {
            out.acts.insert(row, json!({"key": "instrument:figure"}));
        }
    }
    let mut built = vec![];
    for s in array(&page["sections"]) {
        let name = string(&s["name"]);
        let mut blocks: Vec<(Vec<String>, Value)> = vec![];
        for item in array(&s["items"]) {
            let is_open = ui["open"] == item["key"]
                || (item["key"] == "shifts" && super::shiftmap::map_open(&ui["open"]));
            let heart = ui["likes"].as_object().and_then(|likes| {
                let key = item["like"].as_str().unwrap_or(string(&item["key"]));
                (name != "earlier"
                    && item["quiet"] != true
                    && item["unrated"] != true
                    && !string(&item["key"]).starts_with("fold:"))
                .then(|| likes.get(key).and_then(Value::as_bool).unwrap_or(false))
            });
            let item_rows =
                item_rows_mode(item, name, width, is_open, at, heart, (&ui["sent"], pixels));
            blocks.push((item_rows, item.clone()));
            if item["answers"] == true {
                let half = width.saturating_sub(at) / 2;
                for pair in [
                    [
                        ("helpful", "✓ helpful"),
                        ("thought_of_it", "= thought of it"),
                    ],
                    [
                        ("not_relevant", "✗ not relevant"),
                        ("ask_claude", "→ ask Claude"),
                    ],
                ] {
                    let mut text = " ".repeat(at);
                    let mut spans = vec![];
                    for (k, (answer, label)) in pair.iter().enumerate() {
                        let start = at + k * half;
                        spans.push(json!([start, start + half, answer]));
                        let first = label.chars().next().unwrap();
                        let rest = label.chars().skip(1).collect::<String>();
                        let rest = fit(&rest, half.saturating_sub(2));
                        text.push_str(&format!(
                            "{}{BOLD}{first}{OFF}{}{}{}{OFF}",
                            ink(section("dreams")),
                            ink("quiet"),
                            rest,
                            " ".repeat(half.saturating_sub(1 + len(&rest)))
                        ));
                    }
                    blocks.push((
                        vec![text],
                        json!({
                            "key": format!("{}:answers", string(&item["key"])),
                            "dream": item["dream"],
                            "send": item["send"],
                            "spans": spans,
                        }),
                    ));
                }
                // The pane keeps an answer six seconds in place of the two rows
                if ui["answered"][0].is_string() && ui["answered"][0] == item["dream"] {
                    let said = dreams::answer_word(string(&ui["answered"][1]));
                    blocks.truncate(blocks.len() - 2);
                    blocks.push((
                        vec![format!(
                            "{}{}{}{OFF}",
                            " ".repeat(at),
                            ink("faint"),
                            fit(&format!("✓ kept: {said}"), width.saturating_sub(at))
                        )],
                        Value::Null,
                    ));
                }
            }
            blocks.extend(super::lostmain::answer_blocks(
                item,
                width,
                at,
                &ui["sent"],
                is_open,
            ));
        }
        if !blocks.is_empty() {
            let base = 2 + blocks[0].0.len();
            let want = 2 + blocks.iter().map(|(r, _)| r.len()).sum::<usize>();
            built.push((s, blocks, base, want));
        }
    }
    if built.is_empty() {
        out.put(
            wrap(string(&page["quiet"]), width - 2, 3)
                .iter()
                .map(|l| format!(" {}{l}{OFF}", ink("faint")))
                .collect(),
            None,
        );
        return out;
    }
    let mut left = rows as isize
        - out.rows.len() as isize
        - built
            .iter()
            .map(|(_, _, base, _)| *base as isize)
            .sum::<isize>();
    let mut rooms = BTreeMap::new();
    let mut order: Vec<_> = (0..built.len()).collect();
    order.sort_by_key(|i| built[*i].0["name"] != "lost");
    for i in order {
        let (_, _, base, want) = &built[i];
        let extra = (*want - *base).min(left.max(0) as usize);
        rooms.insert(i, *base + extra);
        left -= extra as isize;
    }
    // the items' own pictures (a last session, a compaction's payback) go after every section icon
    let mut wanted = vec![];
    for (k, (s, blocks, _, _)) in built.into_iter().enumerate() {
        let name = string(&s["name"]);
        let mut part = Lines::default();
        part.rows.push(heading_mode(s, width, at, pixels));
        let room = rooms[&k] - 1;
        let opened_at = blocks
            .iter()
            .position(|(_, a)| !a.is_null() && a["key"] == ui["open"]);
        let mut first = 0;
        while let Some(open) = opened_at {
            if first >= open
                || 1 + 1
                    + blocks[first..=open]
                        .iter()
                        .map(|(r, _)| r.len())
                        .sum::<usize>()
                    <= room
            {
                break;
            }
            first += 1;
        }
        if first > 0 {
            part.rows.push(format!("        {DIM}+{first} newer{OFF}"));
        }
        for (i, (mut r, a)) in blocks.into_iter().enumerate().skip(first) {
            if Some(i) == opened_at {
                if part.rows.len() + r.len() > room {
                    let last = r.last().cloned().unwrap_or_default();
                    r.truncate(room.saturating_sub(part.rows.len() + 1).max(1));
                    r.push(last);
                }
            } else if part.rows.len() + r.len() > room {
                break;
            }
            part.put(r, (!a.is_null()).then_some(&a));
        }
        for (i, line) in part.rows.iter_mut().enumerate() {
            if line.contains("\x1b[24m") {
                if let Some(act) = part.acts.get_mut(&i) {
                    act["heart"] = json!([width.saturating_sub(3), width + 1]);
                    act["source"] = json!(name);
                }
            }
            if !line.starts_with(&bg(tint(name, 0.15))) {
                *line = tinted(line, &bg(tint(name, if i == 0 { 0.10 } else { 0.07 })));
            }
        }
        if k > 0 {
            out.rows.push(String::new());
        }
        out.heads.push((out.rows.len(), name.into()));
        let start = out.rows.len();
        if pixels && at >= 5 && start + 2 <= rows.saturating_sub(1) {
            out.pictures.push(Wanted {
                row: start,
                col: 0,
                cols: 5,
                rows: 2,
                picture: Picture::Iso(name.into()),
                backs: vec![
                    tint(name, 0.10),
                    tint(
                        name,
                        if part
                            .rows
                            .get(1)
                            .is_some_and(|r| r.starts_with(&bg(tint(name, 0.15))))
                        {
                            0.15
                        } else {
                            0.07
                        },
                    ),
                ],
            });
        }
        let mut i = 0;
        while pixels && i < part.rows.len() {
            if !part.rows[i].contains(PIC) {
                i += 1;
                continue;
            }
            let mut j = i;
            while j + 1 < part.rows.len() && part.rows[j + 1].contains(PIC) {
                j += 1;
            }
            if let Some(act) = part.acts.get(&i) {
                let picture = if ui["open"] == act["key"] && act["open_picture"].is_object() {
                    &act["open_picture"]
                } else {
                    &act["picture"]
                };
                if picture["rows"].as_u64() == Some((j - i + 1) as u64) {
                    wanted.push(Wanted {
                        row: start + i,
                        col: at,
                        cols: width - at,
                        rows: j - i + 1,
                        picture: Picture::from_item(picture),
                        backs: vec![tint(
                            name,
                            if ui["open"] == act["key"] { 0.15 } else { 0.07 },
                        )],
                    });
                }
            }
            i = j + 1;
        }
        out.rows.extend(part.rows);
        out.acts
            .extend(part.acts.into_iter().map(|(r, a)| (r + start, a)));
    }
    out.pictures.extend(wanted);
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_the_story_row_leads_with_where_the_work_stands() {
        assert_eq!(
            where_it_stands("One. Two. Work stands at: done. Nothing pushed."),
            "Work stands at: done. Nothing pushed."
        );
        assert_eq!(where_it_stands("One. Two. Three."), "Two. Three.");
    }
    #[test]
    fn test_a_narrow_page_never_panics() {
        let p = page_log(&json!({
            "shifts": [],
            "total": 0,
        }));
        for cols in [0, 1, 10, 23, 38, 46] {
            draw_page(&p, cols, 20, &json!({}));
        }
    }
}

#[cfg(test)]
mod default_tests {
    use super::*;
    fn facts() -> Value {
        json!({
            "id": "previous-session",
            "title": "Codemod GitHub link side pane",
            "last": 1800000000,
            "active": 43980,
            "prompts": 126,
            "compacts": 16,
            "learned": 4200,
            "handed": 24,
            "peak": 432000,
            "floor": 73120,
            "series": [[100, 100000], [110, 200000]],
            "typed": [100, "my last words"],
            "said": [100, "Claude words"],
        })
    }
    #[test]
    fn test_a_session_with_nothing_of_its_own_gets_the_default_view() {
        let p = json!({
            "sections": [],
        });
        assert!(default_view(
            &p,
            &json!({
                "id": "s",
                "turn_end": 0
            })
        ));
        assert!(!default_view(
            &p,
            &json!({
                "id": null
            })
        ));
        assert!(!default_view(
            &p,
            &json!({
                "id": "s",
                "turn_end": 5,
                "lost": [{
                    "state": "open",
                    "hidden": false
                }]
            })
        ));
        let p = json!({
            "sections": [
                {
                    "name": "rules",
                    "items": [{
                        "key": "r",
                    }],
                },
                {
                    "name": "lost",
                    "items": [{
                        "key": "l",
                    }],
                },
            ],
        });
        assert!(default_view(
            &p,
            &json!({
                "id": "s",
                "turn_end": 5,
                "lost": [{
                    "state": "carried",
                    "hidden": false
                }]
            })
        ));
        assert!(!default_view(
            &p,
            &json!({
                "id": "s",
                "turn_end": 5,
                "lost": [{
                    "state": "lost",
                    "hidden": false
                }]
            })
        ));
        assert!(default_view(
            &p,
            &json!({
                "id": "s",
                "turn_end": 5,
                "lost": [{
                    "state": "lost",
                    "hidden": "drop"
                }]
            })
        ));
    }
    #[test]
    fn test_a_block_with_no_recap_line_falls_back_to_its_first_line_of_substance() {
        let s = json!({
            "id": "s",
            "told": {
                "at": 1800000000,
                "source": "startup",
                "text": "CSR ENDLESS MEMORY ACTIVE\nThe blocks below are memory\n# \
                    Title\nUseful work on the queue\nFull state: \
                    csr_reflect_on_past(\"conv_old\")",
            },
        });
        let section = told_section(&s).unwrap();
        assert_eq!(section["items"][0]["title"], "Useful work on the queue");
        assert!(string(&section["items"][0]["send"]).contains("The rest: csr_reflect_on_past"));
    }
    #[test]
    fn test_without_pixels_one_row_of_block_characters_stands_for_the_picture() {
        let last = last_section(
            &json!({
                "last_session": facts(),
            }),
            1800003600.,
            30,
        )
        .unwrap();
        let rows = item_rows(&last["items"][0], "last", 44, false, 6, None, &Value::Null);
        let graph = plain(&rows[1]);
        assert_eq!(graph, format!("      {}█", "▄".repeat(37)));
    }
    #[test]
    fn test_opening_a_default_row_keeps_the_picture_and_shows_what_more_csr_knows() {
        let last = last_section(
            &json!({
                "last_session": facts(),
            }),
            1800003600.,
            30,
        )
        .unwrap();
        let item = &last["items"][0];
        assert_eq!(
            item["body"],
            json!([
                "12h13m active · 126 prompts · 16 compacts",
                "CSR learned 4.2k · handed 24× · peak 432k"
            ])
        );
        assert_eq!(item["detail"][1], "You last: “my last words”");
        assert_eq!(item["detail"][2], "Claude last: “Claude words”");
        assert!(string(&item["send"]).contains("compactions, last row"));
        let rows = item_rows(item, "last", 44, true, 6, None, &Value::Null);
        let text = rows.iter().map(|r| plain(r)).collect::<Vec<_>>().join("\n");
        assert!(text.contains("You last:"));
        assert!(text.contains("Claude last:"));
        assert!(text.contains('▄'));
        assert!(text.contains("click again"));
    }
    #[test]
    fn test_a_fresh_session_shows_no_empty_lane_and_no_zero_counter() {
        let data = json!({
            "repo": "/fixture",
            "dreams": [],
            "dreams_on": true,
        });
        let s = json!({
            "id": "s",
            "turn_end": 0,
            "last_session": {},
            "todos": [],
        });
        let page = page_main(&data, &s, 1800000000., &json!({}), None);
        let lines = draw_page(&page, 100, 40, &json!({}));
        let text = lines
            .rows
            .iter()
            .map(|r| plain(r))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!text.contains("0 prompts"));
        assert!(!text.contains("0 learned"));
        assert!(text.contains("First session here"));
        assert_eq!(lines.rows[0].matches("CTX").count(), 1);
    }
}

#[cfg(test)]
mod source_regressions {
    use super::*;
    #[test]
    fn unknown_hook_sources_are_bounded_and_cannot_inject_terminal_controls() {
        let s = json!({"id":"s","told":{"at":1800000000.,
            "source":"\u{1b}[2Jthis is a very long source\nnext", "text":"Useful recap"}});
        let section = told_section(&s).unwrap();
        let right = string(&section["right"]);
        assert!(!right.contains('\u{1b}'));
        assert!(!right.contains('\n'));
        assert!(right.starts_with(&one_line(string(&s["told"]["source"]), 12)));
    }
    #[test]
    fn drawing_and_gauge_modules_leave_queries_and_os_probes_in_sources() {
        assert!(!include_str!("pages.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap()
            .contains("db.query("));
        let math = include_str!("masthead.rs");
        for name in [
            "kept_status",
            "account_limits",
            "csr_status_at",
            "compacting_info",
        ] {
            assert!(!math.contains(&format!("pub fn {name}(")));
        }
    }
}

pub fn lanes_legend(s: &Value, width: usize, dark: bool) -> String {
    let counts = [
        array(&s["learned"]["chunks"]).len(),
        array(&s["gave"]["recalls"])
            .iter()
            .filter_map(Value::as_f64)
            .map(|n| n as i64)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            + array(&s["gave"]["carried"]).len(),
        array(&s["asked"]).len(),
    ];
    let color = |name: &str| {
        if dark {
            fg(match name {
                "learn" | "recall" => [36, 163, 114],
                "asked" => [154, 123, 232],
                _ => [238, 245, 250],
            })
        } else {
            ink(name)
        }
    };
    let mut parts = vec![];
    for (i, (name, mark, said)) in [
        (
            "learn",
            "▮",
            format!("{} learned", super::pyfmt::short(counts[0] as f64)),
        ),
        ("recall", "↑", format!("{} handed to Claude", counts[1])),
        ("asked", "●", format!("{} asked", counts[2])),
    ]
    .into_iter()
    .enumerate()
    {
        if counts[i] > 0 {
            parts.push((name, mark, said));
        }
    }
    while parts.len() > 1 && parts.iter().map(|(_, _, s)| 4 + len(s)).sum::<usize>() - 2 > width {
        parts.pop();
    }
    parts
        .iter()
        .map(|(name, mark, said)| {
            format!(
                "{}{mark}{OFF}{}{said}{OFF}",
                color(name),
                format_args!(" {}", if dark { color("text") } else { ink("quiet") })
            )
        })
        .collect::<Vec<_>>()
        .join("  ")
}

#[cfg(test)]
mod picture_tests {
    use super::*;
    fn fresh() -> (Value, Value) {
        let data = json!({"repo":"/fixture","dreams":[],"dreams_on":true});
        let s = json!({"id":"fresh","turn_end":0,"last_session":null});
        (data, s)
    }
    #[test]
    fn a_sparse_main_page_closes_with_what_csr_can_do_when_there_is_room() {
        let (data, s) = fresh();
        let ui = json!({"rows":60});
        let page = page_main(&data, &s, 1800000060., &ui, None);
        let text = draw_page(&page, 60, 60, &ui)
            .rows
            .iter()
            .map(|r| plain(r))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Ask about past work."), "{text}");
        assert!(
            text.contains("Press l for changes. Press i for the figure."),
            "{text}"
        );
    }
    #[test]
    fn the_closer_does_not_draw_when_the_page_has_no_room_left() {
        let (data, s) = fresh();
        let ui = json!({"rows":8});
        let page = page_main(&data, &s, 1800000060., &ui, None);
        let text = draw_page(&page, 60, 8, &ui)
            .rows
            .iter()
            .map(|r| plain(r))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!text.contains("Ask about past work."), "{text}");
    }
    #[test]
    fn the_closer_leaves_out_what_the_page_shows() {
        let page = json!({"sections":[{"name":"told","items":[{"key":"k"}]}]});
        let lines = closer(&page, 60, 6, 20)
            .iter()
            .map(|l| plain(l))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!lines.contains("Ask about past work."));
        assert!(lines.contains("Press l for changes."));
        assert!(closer(&page, 60, 6, 3).is_empty());
    }
    #[test]
    fn last_session_picture_height_tracks_screen_rows() {
        let data = json!({"repo":"/fixture","dreams":[],"dreams_on":true});
        let s = json!({"id":"fresh","turn_end":0,"last_session":{
            "id":"previous","title":"A small previous session","last":1800000000.,
            "active":0,"prompts":1,"compacts":0,"learned":8,"handed":1,"peak":75000,
            "series":[[100,70000],[110,75000]]
        }});
        for (cols, rows, height) in [(46, 30, 4), (100, 40, 4), (46, 29, 3)] {
            let ui = json!({"pixels":true,"rows":rows});
            let page = page_main(&data, &s, 1800000060., &ui, None);
            let lines = draw_page(&page, cols, rows, &ui);
            let picture = lines
                .pictures
                .iter()
                .find(|p| matches!(p.picture, Picture::Last(_)))
                .unwrap();
            assert_eq!(picture.rows, height, "{cols}x{rows}");
            assert_eq!(picture.row, 4);
            assert!(lines.rows[picture.row..picture.row + height]
                .iter()
                .all(|line| line.contains(PIC)));
            assert!(plain(&lines.rows[picture.row + height]).contains("0m active · 1 prompts"));
            assert!(plain(&lines.rows[picture.row + height + 1])
                .contains("CSR learned 8 · handed 1× · peak 75k"));
        }
    }
    #[test]
    fn pixel_items_reserve_the_chart_and_icons_match_opened_row_grounds() {
        let page = json!({"head":{"title":"Session","facts":"A figure"},"sections":[{
        "name":"last","title":"Last session","right":"","items":[{
            "key":"last:one","title":"One session","picture":{"kind":"last","rows":3,
            "facts":{"series":[[100,100000],[200,190000]],"floor":65000}}
        }]}]});
        let lines = draw_page(&page, 46, 30, &json!({"pixels":true,"open":"last:one"}));
        let icon = lines
            .pictures
            .iter()
            .find(|p| matches!(p.picture, Picture::Iso(_)))
            .unwrap();
        assert_eq!(icon.backs, vec![tint("last", 0.10), tint("last", 0.15)]);
        let figure = lines
            .pictures
            .iter()
            .find(|p| matches!(p.picture, Picture::Last(_)))
            .unwrap();
        assert_eq!((figure.col, figure.cols, figure.rows), (6, 39, 3));
        assert!(lines.rows[figure.row..figure.row + 3]
            .iter()
            .all(|r| r.contains(PIC)));
        assert_eq!(figure.backs, vec![tint("last", 0.15)]);
        let phone = draw_page(&page, 46, 30, &json!({"pixels":true,"phone":true}));
        assert!(phone
            .pictures
            .iter()
            .all(|p| !matches!(p.picture, Picture::Iso(_))));
        let narrow = draw_page(&page, 26, 30, &json!({"pixels":true}));
        assert!(narrow
            .pictures
            .iter()
            .all(|p| !matches!(p.picture, Picture::Last(_))));
        assert!(narrow.rows.iter().any(|r| plain(r).contains('█')));
    }
    #[test]
    fn session_lanes_reserve_three_rows_and_keep_their_ramp_grounds() {
        let page = json!({"head":{"title":"Session","facts":"Four prompts","session":{
            "prompts":[100,200],"replies":[[110,100000],[210,190000]]}},"sections":[]});
        let lines = draw_page(&page, 46, 30, &json!({"pixels":true,"now":220}));
        let lanes = lines
            .pictures
            .iter()
            .find(|p| matches!(p.picture, Picture::Session(_)))
            .unwrap();
        assert_eq!(
            (lanes.row, lanes.col, lanes.cols, lanes.rows),
            (2, 9, 36, 3)
        );
        assert_eq!(lanes.backs, dark_steps(6)[2..5]);
    }
}
