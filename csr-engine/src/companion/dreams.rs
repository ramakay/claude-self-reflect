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
    /// Some: index only this session's dreams (a pane's [`DreamLog::for_session`]).
    only: Option<String>,
}
impl DreamIndex {
    fn apply(&mut self, r: &Value) {
        // After a batch's items took the batch's session: the filter sees each dream's own.
        if r["kind"] == "dream"
            && self
                .only
                .as_ref()
                .is_some_and(|sid| r["session"] != sid.as_str())
        {
            return;
        }
        if r["kind"] == "batch" {
            for item in super::array(&r["dreams"]) {
                let mut item = item.clone();
                if !item.is_object() {
                    continue;
                }
                for key in [
                    "at",
                    "session",
                    "repo",
                    "budget",
                    "threads",
                    "confusing_claude",
                    "should_be",
                    "same_page",
                ] {
                    if item[key].is_null() {
                        item[key] = r[key].clone();
                    }
                }
                item["kind"] = json!("dream");
                self.apply(&item);
            }
            return;
        }
        if r["kind"] == "answer" && r["answer"] == "carried" {
            if let Some(i) = self.positions.get(string(&r["id"])).copied() {
                self.items[i]["carried"] = json!(true);
            }
            return;
        }
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
/// The dream log as read so far. `default()` keeps every row; a pane's
/// [`DreamLog::for_session`] keeps only what that pane draws, which is a few rows out of a log
/// of every session's (8 MB, 34 MB parsed, on one machine).
#[derive(Default)]
pub struct DreamLog {
    tail: super::tail::Tail,
    index: DreamIndex,
    /// The chapter, look, dream and batch rows, in log order. Scoped to a session: that
    /// session's rows, plus, of every other session's look rows, only the newest per repo (and
    /// per no repo), which is all [`last_look`] can return of them.
    pub records: Vec<Value>,
    /// When the completed looks a scoped log let go of were made (8 bytes each, see
    /// [`looks_since`]): the away line counts every session's looks, and the rows are gone.
    looks_gone: Vec<f64>,
}
impl DreamLog {
    /// A log that keeps only `sid`'s dreams and rows. Everything a pane draws from the log is
    /// filtered to its own session, except [`last_look`]; an empty `sid` keeps everything.
    pub fn for_session(sid: &str) -> Self {
        let mut log = Self::default();
        log.index.only = (!sid.is_empty()).then(|| sid.to_owned());
        log
    }
    pub fn poll(&mut self, paths: &Paths) {
        let mut tail = std::mem::take(&mut self.tail);
        let _ = tail.poll(
            &paths.keep.join("dreams.jsonl"),
            usize::MAX,
            || false,
            |line| {
                let Some(line) = line else {
                    self.index = DreamIndex {
                        only: self.index.only.take(),
                        ..DreamIndex::default()
                    };
                    self.records.clear();
                    self.looks_gone.clear();
                    return;
                };
                if let Ok(row) = sonic_rs::from_slice::<Value>(line) {
                    self.index.apply(&row);
                    self.keep_record(row);
                }
            },
        );
        self.tail = tail;
    }
    fn keep_record(&mut self, row: Value) {
        let kind = string(&row["kind"]);
        if !matches!(kind, "chapter" | "look" | "dream" | "batch" | "looking") {
            return;
        }
        let Some(sid) = self.index.only.as_deref() else {
            self.records.push(row);
            return;
        };
        if row["session"] == sid {
            self.records.push(row);
        } else if kind != "chapter" {
            // Another session's look: it replaces the one kept for its repo, at the log's end.
            // The row goes; when it was a completed look its time is kept, for the away count.
            let repo = &row["repo"];
            let gone = &mut self.looks_gone;
            self.records.retain(|r| {
                let kept = r["session"] == sid || r["kind"] == "chapter" || r["repo"] != *repo;
                if !kept && is_completed_look(r) {
                    gone.push(num(&r["at"]));
                }
                kept
            });
            self.records.push(row);
        }
    }
    /// The times of the completed looks this log let go of: pass with the rows to [`looks_since`].
    pub fn look_stamps(&self) -> &[f64] {
        &self.looks_gone
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
/// A look that finished: not one still under way.
fn is_completed_look(row: &Value) -> bool {
    row["kind"] == "look" && !matches!(string(&row["outcome"]), "dreaming" | "started")
}
/// How many completed looks, of any session, were made after `since`: the rows in `records`, and
/// the times in `gone` (a scoped log's [`DreamLog::look_stamps`], as JSON numbers) of the looks
/// whose rows it let go. A look is in one of the two, never both.
pub fn looks_since(records: &[Value], gone: &[Value], since: f64) -> usize {
    let rows = records
        .iter()
        .filter(|r| is_completed_look(r) && num(&r["at"]) > since);
    rows.count() + gone.iter().filter(|at| num(at) > since).count()
}
pub fn last_look(records: &[Value], root: &str) -> Value {
    records
        .iter()
        .rev()
        .find(|r| {
            matches!(string(&r["kind"]), "look" | "dream" | "batch" | "looking")
                && (r["repo"].is_null() || r["repo"] == root)
        })
        .cloned()
        .unwrap_or(Value::Null)
}
pub fn dreams_on(paths: &Paths) -> bool {
    dreams_on_in(&read_json(&paths.home.join(".claude/settings.json")))
}
/// [`dreams_on`] from settings.json already read.
pub fn dreams_on_in(settings: &Value) -> bool {
    !settings["pluginConfigs"]
        .as_object()
        .into_iter()
        .flatten()
        .any(|(name, c)| {
            name.split('@').next() == Some("csr-companion") && c["options"]["dreams"] == false
        })
}
/// What every surface says while `CSR_NO_DREAMING` has stopped dreams. The variable wins over the
/// `/config` Dreams row, whether the row is on or off.
pub const STOPPED: &str = "Dreams are stopped. CSR_NO_DREAMING is set where Claude Code runs. \
Unset it and start Claude Code again to dream.";
/// The card title's short form: it keeps the variable's name.
pub const STOPPED_TAG: &str = "Stopped by CSR_NO_DREAMING";
/// Whether the app found `CSR_NO_DREAMING` set: it writes the fact into the session's usage file
/// (`usage::measure`), and the pane's own process (herdr starts it, so its environment is not
/// Claude Code's) reads it back as `claude_fields.dreams_stopped`.
pub fn stopped(data: &Value) -> bool {
    data["claude_fields"]["dreams_stopped"] == true
}
/// Whether the pane offers to turn dreams on: the row is off and the variable is not set. A press
/// cannot help under the variable.
pub fn offered(data: &Value) -> bool {
    data["dreams_on"].as_bool() == Some(false) && !stopped(data)
}
/// The notice under the dreamer card: the stopped words under the variable, else the pane's own.
pub fn offer_notice(data: &Value) -> Option<String> {
    if stopped(data) {
        return Some(STOPPED.to_owned());
    }
    data["offer_notice"].as_str().map(str::to_owned)
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
    let mut pending: Vec<_> = mine
        .iter()
        .filter(|d| d["answer"].is_null() && (num(&d["at"]) >= start || d["carried"] == true))
        .cloned()
        .collect();
    pending.sort_by(|a, b| {
        num(&b["confidence"])
            .total_cmp(&num(&a["confidence"]))
            .then_with(|| num(&b["at"]).total_cmp(&num(&a["at"])))
    });
    let star = pending.first().cloned().unwrap_or(Value::Null);
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
        "queue": pending.iter().skip(1).collect::<Vec<_>>(),
        "budget": star["budget"],
        "star": star,
        "adopted": adopted,
        "folded": folded,
        "passed": passed,
    })
}
pub fn answer_word(word: &str) -> &str {
    match word {
        "ask_claude" => "asked the agent",
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

#[cfg(test)]
mod scoped_tests {
    use super::*;
    fn write(paths: &Paths, rows: &[Value]) {
        std::fs::create_dir_all(&paths.keep).unwrap();
        let text: String = rows.iter().map(|r| format!("{r}\n")).collect();
        std::fs::write(paths.keep.join("dreams.jsonl"), text).unwrap();
    }
    fn rows() -> Vec<Value> {
        let mut rows = vec![
            json!({"kind":"chapter","session":"a","repo":"/r","at":1,"of":4}),
            json!({"kind":"dream","session":"a","repo":"/r","id":"a1","line":"A?","at":2}),
            json!({"kind":"batch","session":"b","repo":"/r","at":3,
                "dreams":[{"id":"b1","line":"B?"}]}),
            json!({"kind":"chapter","session":"b","repo":"/r","at":3,"of":5}),
            json!({"kind":"look","session":"a","repo":"/r","at":4,"outcome":"none"}),
            json!({"kind":"answer","id":"a1","answer":"helpful"}),
            json!({"kind":"answer","id":"b1","answer":"helpful"}),
        ];
        // Another session looks again and again, in this repo, in another and in none.
        for i in 0..300 {
            let repo = [json!("/r"), json!("/s"), Value::Null][i % 3].clone();
            rows.push(json!({"kind":"look","session":"b","repo":repo,"at":5 + i,"outcome":"none"}));
        }
        rows
    }
    #[test]
    fn another_session_s_dream_batch_and_looking_rows_count_as_its_newest_row_of_the_repo() {
        // `last_look` answers with any of these kinds, so a scoped log keeps the newest of them.
        let mut rows = rows();
        rows.push(json!({"kind":"dream","session":"b","repo":"/r","id":"b2","line":"?","at":900}));
        rows.push(json!({"kind":"batch","session":"c","repo":"/s","at":901,
            "dreams":[{"id":"c1","line":"C?"}]}));
        rows.push(json!({"kind":"looking","session":"b","repo":"/r","at":902}));
        rows.push(json!({"kind":"look","session":"b","repo":null,"at":903}));
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().into());
        write(&paths, &rows);
        let mut all = DreamLog::default();
        all.poll(&paths);
        let mut mine = DreamLog::for_session("a");
        mine.poll(&paths);
        for root in ["/r", "/s", "/t", ""] {
            assert_eq!(
                last_look(&mine.records, root),
                last_look(&all.records, root),
                "{root}"
            );
        }
        assert!(mine.records.len() < 10, "{}", mine.records.len());
    }

    fn stamps(log: &DreamLog) -> Vec<Value> {
        log.look_stamps().iter().map(|at| json!(at)).collect()
    }

    #[test]
    fn two_looks_of_another_session_in_one_repo_are_two_looks_in_the_away_count() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().into());
        write(
            &paths,
            &[
                json!({"kind":"look","session":"b","repo":"/r","at":100,"outcome":"none"}),
                json!({"kind":"look","session":"b","repo":"/r","at":200,"outcome":"none"}),
            ],
        );
        let mut mine = DreamLog::for_session("a");
        mine.poll(&paths);
        // The bound still holds: one row for the repo.
        assert_eq!(mine.records.len(), 1);
        assert_eq!(looks_since(&mine.records, &stamps(&mine), 50.), 2);
        assert_eq!(looks_since(&mine.records, &stamps(&mine), 150.), 1);
        assert_eq!(looks_since(&mine.records, &stamps(&mine), 250.), 0);
    }

    #[test]
    fn a_pane_s_away_count_is_the_whole_log_s_for_any_start() {
        // Looks of other sessions in one repo, in two, in none; two sessions in one repo; a look
        // still under way; and a later row of another kind that takes a look's place.
        let rows = [
            json!({"kind":"look","session":"b","repo":"/r","at":10,"outcome":"none"}),
            json!({"kind":"look","session":"c","repo":"/r","at":20,"outcome":"dreamed"}),
            json!({"kind":"look","session":"b","repo":"/s","at":30,"outcome":"none"}),
            json!({"kind":"look","session":"b","repo":"/s","at":40,"outcome":"started"}),
            json!({"kind":"look","session":"b","repo":null,"at":50,"outcome":"none"}),
            json!({"kind":"look","session":"b","repo":null,"at":60,"outcome":"dreaming"}),
            json!({"kind":"look","session":"a","repo":"/r","at":70,"outcome":"none"}),
            json!({"kind":"look","session":"b","repo":"/r","at":80,"outcome":"none"}),
            json!({"kind":"looking","session":"c","repo":"/s","at":90}),
            json!({"kind":"dream","session":"c","repo":"/r","id":"c9","line":"C?","at":95}),
            json!({"kind":"look","session":"c","repo":"/r","at":100,"outcome":"none"}),
        ];
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().into());
        write(&paths, &rows);
        let mut all = DreamLog::default();
        all.poll(&paths);
        let mut mine = DreamLog::for_session("a");
        mine.poll(&paths);
        assert!(mine.records.len() < all.records.len());
        for since in [0., 10., 15., 25., 35., 45., 55., 65., 75., 85., 95., 100.] {
            assert_eq!(
                looks_since(&mine.records, &stamps(&mine), since),
                looks_since(&all.records, &[], since),
                "since {since}"
            );
        }
        // Seven completed looks in all: the two looks "started" and "dreaming" are not looks.
        assert_eq!(looks_since(&mine.records, &stamps(&mine), 0.), 7);
        // A rotated log forgets them with its rows.
        std::fs::write(paths.keep.join("dreams.jsonl"), "").unwrap();
        mine.poll(&paths);
        assert!(mine.look_stamps().is_empty());
    }

    #[test]
    fn a_pane_s_log_holds_its_own_session_and_one_look_per_repo_and_draws_the_same() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().into());
        write(&paths, &rows());
        let mut all = DreamLog::default();
        all.poll(&paths);
        let mut mine = DreamLog::for_session("a");
        mine.poll(&paths);
        // Bound: a's three rows and the newest look of b in each of /r, /s and no repo.
        assert_eq!(all.records.len(), 305);
        assert_eq!(mine.records.len(), 6);
        assert_eq!(mine.dreams("").len(), 1);
        assert_eq!(mine.dreams("")[0]["answer"], "helpful");
        for root in ["/r", "/s", "/t", ""] {
            assert_eq!(
                last_look(&mine.records, root),
                last_look(&all.records, root),
                "{root}"
            );
            assert_eq!(
                last_chapter(&mine.records, root, "a"),
                last_chapter(&all.records, root, "a")
            );
            let own = |log: &DreamLog| -> Vec<Value> {
                let dreams = log.dreams(root);
                dreams.into_iter().filter(|d| d["session"] == "a").collect()
            };
            assert_eq!(own(&mine), own(&all), "{root}");
        }
        // A rotated log starts over and keeps its scope.
        std::fs::write(paths.keep.join("dreams.jsonl"), "").unwrap();
        mine.poll(&paths);
        assert!(mine.records.is_empty());
        write(&paths, &rows());
        mine.poll(&paths);
        assert_eq!(mine.records.len(), 6);
        assert_eq!(DreamLog::for_session("").index.only, None);
    }
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    #[test]
    fn batches_rank_promote_and_carry_without_inventing_metadata() {
        let rows = vec![
            json!({"kind":"batch","session":"s","at":1,"budget":{"spent":14,"cap":40},
            "dreams":[{"id":"a","line":"What if A?","confidence":0.4},
                      {"id":"b","line":"What if B?","confidence":0.8}]}),
            json!({"kind":"answer","id":"b","answer":"helpful"}),
            json!({"kind":"answer","id":"a","answer":"carried"}),
        ];
        let dreams = load_dream_log(&rows, "");
        let f = feed(&dreams, &Value::Null, "s");
        assert_eq!(f["star"]["id"], "a");
        assert_eq!(f["star"]["carried"], true);
        assert_eq!(f["budget"]["cap"], 40);
        assert!(f["star"]["receipt"].is_null());
    }
}
