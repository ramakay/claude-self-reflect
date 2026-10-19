use super::{
    array,
    db::Db,
    num,
    paths::{epoch, Paths},
    render::text::*,
    send, string,
};
use chrono::{Local, TimeZone};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub fn where_it_stands(text: &str) -> String {
    let mut sentences = vec![];
    let mut at = 0;
    for (i, c) in text.char_indices() {
        if c.is_whitespace()
            && i > 0
            && text[..i].ends_with(['.', '!', '?'])
            && text[i..]
                .trim_start()
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_uppercase() || c == '`')
        {
            sentences.push(text[at..i].trim());
            at = i;
        }
    }
    sentences.push(text[at..].trim());
    let re = regex::Regex::new(r"(?i)\b(?:stands?|stood|standing)\b").unwrap();
    let first = sentences
        .iter()
        .rposition(|s| re.is_match(s))
        .unwrap_or(sentences.len().saturating_sub(2));
    sentences[first..].join(" ")
}
pub fn session_story(db: &Db, paths: &Paths, sid: &str) -> Value {
    for id in super::session::lineage(paths, sid) {
        let rows = db.query(
            "select content,tags,timestamp from reflections where tags like \
                ? and tags like '%session_story%' order by timestamp desc limit \
                1",
            &[json!(format!("%conv_{id}%"))],
        );
        let Some(r) = rows.first() else {
            continue;
        };
        let text = one_line(string(&r[0]), usize::MAX);
        if text.is_empty() {
            continue;
        }
        let tags: Value = serde_json::from_str(string(&r[1])).unwrap_or(Value::Null);
        let rev = array(&tags)
            .iter()
            .find_map(|t| {
                string(t)
                    .strip_prefix("story_rev_")
                    .and_then(|s| s.parse::<u64>().ok())
            })
            .unwrap_or(1);
        let by = if array(&tags).contains(&json!("story_by_haiku")) {
            "Haiku"
        } else if array(&tags).contains(&json!("story_extracted")) {
            "extract"
        } else {
            ""
        };
        let mut story = json!({
            "text": text,
            "now": where_it_stands(&text),
            "rev": rev,
            "by": by,
            "at": epoch(string(&r[2])),
        });
        if id != sid {
            story["from_sid"] = json!(id);
        }
        return story;
    }
    Value::Null
}
fn gist(text: &str) -> String {
    let text = one_line(&text.replace('`', ""), 700);
    let text = text
        .strip_prefix("The session ")
        .or_else(|| text.strip_prefix("This session "))
        .unwrap_or(&text);
    let first = text
        .split(". ")
        .next()
        .unwrap_or(text)
        .trim_end_matches([' ', '.']);
    let mut chars = first.chars();
    let Some(c) = chars.next() else {
        return String::new();
    };
    c.to_uppercase().collect::<String>() + chars.as_str()
}
pub fn rules(db: &Db, s: &Value, root: &str) -> (Vec<Value>, f64) {
    let sid = string(&s["id"]);
    let mut rules = vec![];
    let mut rules_at = 0_f64;
    {
        let project = string(&s["project"]);
        let basename = std::path::Path::new(root)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        for r in db.query(
            "select ts,quote,session_id,transcript_path,byte_start from \
                intent_events where project in (?,?) and kind='correction' \
                order by ts desc limit 200",
            &[json!(project), json!(basename)],
        ) {
            if rules.len() >= 3 {
                break;
            }
            let quote = one_line(string(&r[1]), 700);
            if !send::typed_by_you(
                std::path::Path::new(string(&r[3])),
                num(&r[4]) as u64,
                Some(&quote),
            ) {
                continue;
            }
            let at = epoch(string(&r[0])).unwrap_or(0.);
            rules_at = rules_at.max(at);
            let day = Local
                .timestamp_opt(at as i64, 0)
                .single()
                .map(|v| v.format("%m-%d").to_string())
                .unwrap_or_default();
            rules.push(json!({
                "key": format!(
                    "rule:{}:{}",
                    string(&r[0]),
                    quote.chars().take(30).collect::<String>()
                ),
                "title": quote,
                "meta": format!(
                    "you said · {day}{}",
                    if r[2] == sid { " · this session" } else { "" }
                ),
                "detail": [format!(
                    "Your words, session {}. CSR hands them to Claude when the work \
                        comes near.",
                    string(&r[2]).chars().take(8).collect::<String>()
                )],
                "send": one_line(
                    &format!("A standing rule from {day}: “{quote}”"),
                    700,
                ),
            }));
        }
    }
    (rules, rules_at)
}

pub fn remembered(db: &Db, s: &Value) -> Vec<Value> {
    let sid = string(&s["id"]);
    let rows = db.query(
        "select r.retrieved_at,r.memory_type,r.memory_id,\
            c.conversation_id,p.source_conv_id,c.timestamp from \
            retrieval_events r left join chunks c on c.id=r.memory_id left \
            join chunk_provenance p on p.chunk_id=r.memory_id where \
            r.session_id=? order by r.retrieved_at",
        &[json!(sid)],
    );
    let uuid = regex::Regex::new(r"^[0-9a-f-]{36}$").unwrap();
    let mut groups: BTreeMap<String, Value> = BTreeMap::new();
    for r in rows {
        let mut conversation = if string(&r[3]).starts_with("agent-") {
            string(&r[4]).to_owned()
        } else {
            string(&r[3]).to_owned()
        };
        let memory = string(&r[2]);
        let reflection = r[1] == "reflection";
        if reflection {
            if let Some(id) = memory
                .strip_prefix("extracted_v3_")
                .or_else(|| memory.strip_prefix("story_"))
                .filter(|id| uuid.is_match(id))
            {
                conversation = id.into();
            } else {
                conversation = format!("reflection:{memory}");
            }
        }
        if conversation.is_empty() || conversation == sid {
            continue;
        }
        let moment = epoch(string(&r[0])).unwrap_or(0.);
        let item = groups.entry(conversation.clone()).or_insert_with(|| {
            json!({
                "key": conversation,
                "title": "",
                "memory": memory,
                "when": null,
                "project": "",
                "at": moment,
                "first": moment,
                "seconds": [],
            })
        });
        if moment > num(&item["at"]) {
            item["at"] = json!(moment);
        }
        if moment < num(&item["first"]) {
            item["first"] = json!(moment);
        }
        let second = json!(moment as i64);
        if moment != 0. && !array(&item["seconds"]).contains(&second) {
            item["seconds"].as_array_mut().unwrap().push(second);
        }
    }
    for (conversation, item) in &mut groups {
        let memory = string(&item["memory"]);
        let (title, when, project) = if conversation.starts_with("reflection:") {
            let rows = db.query(
                "select content,timestamp from reflections where id=?",
                &[json!(memory)],
            );
            let Some(r) = rows.first() else {
                continue;
            };
            (gist(string(&r[0])), epoch(string(&r[1])), String::new())
        } else {
            let rows = db.query(
                "select first_prompt,first_ts,last_ts,project from \
                    session_registry where session_id=?",
                &[json!(conversation)],
            );
            let seen = rows.first();
            let story = db.scalar(
                "select content from reflections where tags like ? and tags like \
                    '%session_story%' order by (id like 'story_%'), timestamp desc \
                    limit 1",
                &[json!(format!("%conv_{conversation}%"))],
            );
            let extracted = if story.is_null() {
                db.scalar(
                    "select content from reflections where id=?",
                    &[json!(format!("extracted_v3_{conversation}"))],
                )
            } else {
                Value::Null
            };
            let title = story
                .as_str()
                .or(extracted.as_str())
                .or_else(|| {
                    seen.and_then(|r| r[0].as_str())
                        .filter(|s| !s.trim_start().starts_with(['[', '<', '/']))
                })
                .unwrap_or("");
            (
                gist(title),
                seen.and_then(|r| {
                    epoch(if string(&r[2]).is_empty() {
                        string(&r[1])
                    } else {
                        string(&r[2])
                    })
                }),
                seen.map(|r| string(&r[3]).to_owned()).unwrap_or_default(),
            )
        };
        item["title"] = json!(title);
        item["when"] = json!(when);
        item["project"] = json!(project);
    }
    let mut recalled: Vec<_> = groups
        .into_values()
        .filter(|i| {
            let id = string(&i["key"]);
            !string(&i["title"]).is_empty()
                && uuid.is_match(id)
                && array(&s["questions"])
                    .iter()
                    .any(|q| num(&q[0]) >= num(&i["first"]) && string(&q[2]).contains(&id[..8]))
        })
        .collect();
    recalled.sort_by(|a, b| num(&b["at"]).total_cmp(&num(&a["at"])));
    recalled
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repeated_retrievals_look_up_each_conversation_once_per_frame() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().into());
        std::fs::create_dir_all(paths.db.parent().unwrap()).unwrap();
        let con = rusqlite::Connection::open(&paths.db).unwrap();
        con.execute_batch(include_str!("../../tests/fixtures/companion/home.sql"))
            .unwrap();
        let id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        con.execute(
            "insert into session_registry values(?1,'project','Past work',?2,?2)",
            [id, "2026-10-18T00:00:00Z"],
        )
        .unwrap();
        for i in 0..40 {
            let key = format!("c{i}");
            con.execute(
                "insert into chunks(id,conversation_id) values(?1,?2)",
                [&key, id],
            )
            .unwrap();
            con.execute(
                "insert into retrieval_events values('live',?1,'chunk',?2)",
                ["2026-10-18T01:00:00Z", &key],
            )
            .unwrap();
        }
        drop(con);
        let db = Db::open(&paths).unwrap();
        let s = json!({"id":"live","questions":[[1800000000,"recall",id]]});
        let rows = remembered(&db, &s);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["title"], "Past work");
        assert_eq!(db.queries.load(std::sync::atomic::Ordering::Relaxed), 4);
    }
}
