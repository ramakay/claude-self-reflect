use super::{
    num,
    paths::{read_json, Paths},
    private, string,
};
use serde_json::{json, Value};
use std::collections::HashMap;
pub const SETTLED: &[&str] = &[
    "helpful",
    "not_relevant",
    "ask_claude",
    "thought_of_it",
    "got_it",
    "dismiss",
    "ignored",
    "turn_off",
    "expired",
    "sent",
    "rejected",
];
pub const ADOPTED: &[&str] = &["helpful", "ask_claude", "sent"];
pub fn keep(paths: &Paths, record: &Value) -> std::io::Result<()> {
    private::append(
        &paths.keep.join("dreams.jsonl"),
        &(private::dumps_record(record) + "\n"),
        &paths.keep,
    )
}
#[derive(Default)]
struct DreamIndex {
    positions: HashMap<String, usize>,
    items: Vec<Value>,
}
impl DreamIndex {
    fn apply(&mut self, r: &Value) {
        if r["kind"] == "dream"
            && r["id"].is_string()
            && r["line"].as_str().is_some_and(|s| !s.is_empty())
        {
            let mut d = r.clone();
            d["answer"] = Value::Null;
            if d["at"].is_null() {
                d["at"] = json!(0.);
            }
            if let Some(i) = self.positions.get(string(&r["id"])).copied() {
                self.items[i] = d;
            } else {
                self.positions
                    .insert(string(&r["id"]).to_owned(), self.items.len());
                self.items.push(d);
            }
        } else if r["kind"] == "answer" && SETTLED.contains(&string(&r["answer"])) {
            if let Some(d) = self
                .positions
                .get(string(&r["id"]))
                .map(|i| &mut self.items[*i])
            {
                let a = string(&d["answer"]);
                let next = string(&r["answer"]);
                if matches!(a, "" | "dismiss" | "got_it" | "ignored")
                    || matches!(next, "helpful" | "not_relevant" | "rejected")
                    || (next == "sent" && matches!(a, "ask_claude" | "expired"))
                {
                    d["answer"] = r["answer"].clone();
                    d["said"] = if super::truthy(&r["said"]) {
                        json!(super::render::text::one_line(string(&r["said"]), 300))
                    } else {
                        Value::Null
                    };
                }
            }
        }
    }
}
#[derive(Default)]
pub struct DreamLog {
    tail: super::tail::Tail,
    index: DreamIndex,
    pub records: Vec<Value>,
}
impl DreamLog {
    pub fn poll(&mut self, paths: &Paths) {
        let mut tail = std::mem::take(&mut self.tail);
        let _ = tail.poll(
            &paths.keep.join("dreams.jsonl"),
            usize::MAX,
            || false,
            |line| {
                let Some(line) = line else {
                    self.index = DreamIndex::default();
                    self.records.clear();
                    return;
                };
                if let Ok(row) = sonic_rs::from_slice::<Value>(line) {
                    self.index.apply(&row);
                    if matches!(string(&row["kind"]), "chapter" | "look" | "dream") {
                        self.records.push(json!({"kind":row["kind"],"at":row["at"],
                        "repo":row["repo"],"session":row["session"],"of":row["of"],
                        "outcome":row["outcome"],"every":row["every"]}));
                    }
                }
            },
        );
        self.tail = tail;
    }
    pub fn dreams(&self, root: &str) -> Vec<Value> {
        let mut dreams: Vec<_> = self
            .index
            .items
            .iter()
            .filter(|d| root.is_empty() || d["repo"].is_null() || d["repo"] == root)
            .cloned()
            .collect();
        dreams.sort_by(|a, b| num(&b["at"]).total_cmp(&num(&a["at"])));
        dreams
    }
}
pub fn load_dream_log(records: &[Value], root: &str) -> Vec<Value> {
    let mut index = DreamIndex::default();
    for r in records {
        index.apply(r);
    }
    let mut dreams = index.items;
    dreams.retain(|d| root.is_empty() || d["repo"].is_null() || d["repo"] == root);
    dreams.sort_by(|a, b| num(&b["at"]).total_cmp(&num(&a["at"])));
    dreams
}
pub fn last_chapter(records: &[Value], root: &str, sid: &str) -> Value {
    records
        .iter()
        .rev()
        .find(|r| {
            r["kind"] == "chapter"
                && r["session"] == sid
                && (r["repo"].is_null() || r["repo"] == root)
        })
        .map(|r| {
            json!({
                "at": num(&r["at"]),
                "of": r["of"].as_u64().filter(|n| *n > 0).unwrap_or(5),
            })
        })
        .unwrap_or(Value::Null)
}
pub fn last_look(records: &[Value], root: &str) -> Value {
    records
        .iter()
        .rev()
        .find(|r| {
            matches!(string(&r["kind"]), "look" | "dream")
                && (r["repo"].is_null() || r["repo"] == root)
        })
        .map(|r| {
            json!({
                "at": num(&r["at"]),
                "outcome": r["outcome"].as_str().unwrap_or("dream"),
                "every": r["every"],
            })
        })
        .unwrap_or(Value::Null)
}
pub fn dreams_on(paths: &Paths) -> bool {
    let settings = read_json(&paths.home.join(".claude/settings.json"));
    !settings["pluginConfigs"]
        .as_object()
        .into_iter()
        .flatten()
        .any(|(name, c)| {
            name.split('@').next() == Some("csr-companion") && c["options"]["dreams"] == false
        })
}
pub fn feed(dreams: &[Value], chapter: &Value, sid: &str) -> Value {
    let mine: Vec<_> = dreams
        .iter()
        .filter(|d| d["session"] == sid)
        .cloned()
        .collect();
    let start = num(&chapter["at"]);
    let of = chapter["of"].as_u64().unwrap_or(5) as usize;
    let current: Vec<_> = mine.iter().filter(|d| num(&d["at"]) >= start).collect();
    let count = current
        .iter()
        .filter(|d| ADOPTED.contains(&string(&d["answer"])))
        .count();
    let star = mine
        .first()
        .filter(|d| d["answer"].is_null() && num(&d["at"]) >= start)
        .cloned()
        .unwrap_or(Value::Null);
    let adopted: Vec<_> = mine
        .iter()
        .filter(|d| ADOPTED.contains(&string(&d["answer"])))
        .take(2)
        .cloned()
        .collect();
    let folded: Vec<_> = mine
        .iter()
        .filter(|d| *d != &star && !adopted.contains(d))
        .cloned()
        .collect();
    let passed: Vec<_> = folded
        .iter()
        .filter(|d| !ADOPTED.contains(&string(&d["answer"])))
        .cloned()
        .collect();
    json!({
        "left": (of + count).saturating_sub(current.len()),
        "of": of + count,
        "star": star,
        "adopted": adopted,
        "folded": folded,
        "passed": passed,
    })
}
pub fn answer_word(word: &str) -> &str {
    match word {
        "ask_claude" => "asked Claude",
        "thought_of_it" => "you'd thought of it",
        "not_relevant" => "not relevant",
        "got_it" => "read",
        "dismiss" => "dismissed",
        "ignored" => "passed",
        "turn_off" => "turned dreams off",
        "expired" => "folded at a compaction",
        "sent" => "sent to Claude",
        "rejected" => "you said no",
        s => s,
    }
}
#[cfg(test)]
mod tests {
    use super::super::array;
    use super::*;
    fn d(at: f64, answer: Value) -> Value {
        json!({
            "id": at.to_string(),
            "at": at,
            "answer": answer,
            "session": "s",
            "line": "What if?",
        })
    }
    #[test]
    fn test_left_counts_this_chapter_and_an_adopted_dream_gives_its_place_back() {
        let f = feed(
            &[
                d(1300., json!("ask_claude")),
                d(1200., json!("ignored")),
                d(900., json!("not_relevant")),
            ],
            &json!({
                "at": 1000,
                "of": 5,
            }),
            "s",
        );
        assert_eq!((f["left"].clone(), f["of"].clone()), (json!(4), json!(6)));
        assert!(f["star"].is_null());
    }
    #[test]
    fn test_the_star_is_the_newest_dream_still_open_in_this_chapter() {
        let c = json!({
            "at": 1000,
            "of": 5,
        });
        assert_eq!(feed(&[d(1500., Value::Null)], &c, "s")["star"]["at"], 1500.);
        assert!(feed(&[d(900., Value::Null)], &c, "s")["star"].is_null());
    }
    #[test]
    fn test_dreams_of_other_sessions_never_show() {
        assert_eq!(
            feed(&[d(1500., Value::Null)], &Value::Null, "other")["left"],
            5
        );
    }
    #[test]
    fn test_no_chapter_yet_means_five_left() {
        assert_eq!(feed(&[], &Value::Null, "s")["left"], 5);
    }
    #[test]
    fn test_an_adopted_dream_is_never_counted_as_passed() {
        let f = feed(
            &[
                d(1400., json!("ask_claude")),
                d(900., json!("helpful")),
                d(800., json!("ask_claude")),
                d(700., json!("ignored")),
            ],
            &json!({
                "at": 1000,
                "of": 5,
            }),
            "s",
        );
        assert_eq!(array(&f["passed"]).len(), 1);
        assert_eq!(array(&f["folded"]).len(), 2);
    }
}

#[cfg(test)]
mod incremental_tests {
    use super::*;
    #[test]
    fn dream_log_reads_only_new_bytes_and_resets_after_rotation() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().into());
        let file = paths.keep.join("dreams.jsonl");
        std::fs::create_dir_all(&paths.keep).unwrap();
        std::fs::write(
            &file,
            "{\"kind\":\"dream\",\"id\":\"a\",\"line\":\"one\"}\n",
        )
        .unwrap();
        let mut log = DreamLog::default();
        log.poll(&paths);
        let offset = log.tail.offset;
        log.poll(&paths);
        assert_eq!(log.tail.offset, offset);
        assert_eq!(log.dreams("").len(), 1);
        assert_eq!(log.records.len(), 1);
        use std::io::Write;
        writeln!(
            std::fs::OpenOptions::new()
                .append(true)
                .open(&file)
                .unwrap(),
            "{{\"kind\":\"answer\",\"id\":\"a\",\"answer\":\"helpful\"}}"
        )
        .unwrap();
        log.poll(&paths);
        assert_eq!(log.dreams("")[0]["answer"], "helpful");
        assert_eq!(log.records.len(), 1);
        std::fs::rename(&file, paths.keep.join("old")).unwrap();
        std::fs::write(
            &file,
            "{\"kind\":\"dream\",\"id\":\"b\",\"line\":\"two\"}\n",
        )
        .unwrap();
        log.poll(&paths);
        assert_eq!(log.dreams("")[0]["id"], "b");
        std::fs::write(&file, "").unwrap();
        log.poll(&paths);
        assert!(log.dreams("").is_empty());
    }
}
