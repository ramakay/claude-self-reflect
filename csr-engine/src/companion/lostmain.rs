//! Lost and found on the main page: the section of threads that went quiet or wait on you, and
//! the answers under a row (pane.py's `lost_sections`, `lost_rows`; `lost_asks` and the click are
//! in `threads.rs` and `pane.rs`). The threads are `s["lost"]`, which `carry.rs` fills.
use super::{array, num, recall::date, string, threads, truthy};
use serde_json::{json, Value};

/// On a phone, where the pane is the whole screen and tall, a section shows this many times the
/// items it caps.
pub const PHONE_MORE: usize = 2;

const WHEN: &str = "%m-%d %H:%M";

/// The threads Lost and found shows: not set aside, and lost or open (a thread's rank is its kind).
fn shown(s: &Value) -> Vec<&Value> {
    array(&s["lost"])
        .iter()
        .filter(|t| !truthy(&t["hidden"]) && t["rank"].as_u64().unwrap_or(9) < 3)
        .collect()
}

/// The Lost and found section: the top `most` threads that went quiet or wait on you, once there
/// is any.
pub fn lost_sections(s: &Value, now: f64, most: usize) -> Vec<Value> {
    let every = array(&s["lost"]);
    let shown = shown(s);
    let mut items = vec![];
    for (k, t) in shown.iter().take(most).enumerate() {
        let asks = threads::lost_asks(t);
        let state = string(&t["state"]);
        let times = t["times"].as_u64().unwrap_or(0);
        let lead = if state == "carried" {
            format!("from {}", string(&t["came_from"]))
        } else if state == "lost" {
            format!("quiet {}", super::pages::stamp(num(&t["last_seen"]), now))
        } else {
            "open".to_owned()
        };
        let meta = [
            lead,
            if times > 0 {
                format!("{times}×")
            } else {
                String::new()
            },
        ]
        .into_iter()
        .filter(|x| !x.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
        let flag = if truthy(&t["unlinked"]) {
            "handoff not linked"
        } else if truthy(&t["failed_at"]) {
            "its subagent failed"
        } else if truthy(&t["waiting"]) {
            "waiting on you"
        } else {
            ""
        };
        let why = if truthy(&t["why"]) {
            string(&t["why"]).to_owned()
        } else {
            format!(
                "First appeared {}, last {}, carried through {times} {}.",
                date(num(&t["first_seen"]), WHEN),
                date(num(&t["last_seen"]), WHEN),
                if times == 1 {
                    "compaction"
                } else {
                    "compactions"
                }
            )
        };
        let mut detail = vec![why];
        detail.extend(
            array(&t["detail"])
                .iter()
                .take(3)
                .map(|d| string(d).to_owned()),
        );
        items.push(json!({
            "key": format!("lost:{}", string(&t["key"])),
            "title": t["text"],
            "lines": 2,
            "meta": meta,
            "lost": t["key"],
            "top": k == 0,
            "flag": flag,
            "asks": asks,
            "send": asks["side"],
            "detail": detail,
        }));
    }
    if items.is_empty() {
        return vec![];
    }
    let top = &shown[..shown.len().min(most)];
    let at = top.iter().map(|t| num(&t["last_seen"])).fold(0., f64::max);
    vec![json!({
        "name": "lost",
        "title": "Lost and found",
        "right": format!("{} quiet · f all {}", shown.len(), every.len()),
        "items": items,
        "empty": "",
        "count": shown.len(),
        "at": if at > 0. { json!(at) } else { Value::Null },
        "waits": top.iter().any(|t| truthy(&t["waiting"])),
        "brief": format!("{} quiet", shown.len()),
    })]
}

/// The default view's Lost and found: the same rows, headed "Left over", with how many wait on you.
pub fn left_over(s: &Value, now: f64) -> Option<Value> {
    let mut section = lost_sections(s, now, 3).into_iter().next()?;
    let every = array(&s["lost"]);
    let waiting = every
        .iter()
        .filter(|t| !truthy(&t["hidden"]) && truthy(&t["waiting"]))
        .count();
    section["title"] = json!("Left over");
    section["right"] = json!(format!("{waiting} waiting · f all {}", every.len()));
    Some(section)
}

/// A thread's answers under its row (the top thread's always, any other's once opened): one
/// block per row, so a click knows the thread and, by its column, the answer. A row with no act
/// has a null one.
pub fn answer_blocks(
    item: &Value,
    width: usize,
    at: usize,
    sent: &Value,
    is_open: bool,
) -> Vec<(Vec<String>, Value)> {
    if !truthy(&item["lost"]) || !(item["top"] == true || is_open) {
        return vec![];
    }
    threads::lost_rows(item, width, at, sent)
        .into_iter()
        .map(|(row, act)| (vec![row], act.unwrap_or(Value::Null)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(key: &str, state: &str, rank: u64, waiting: bool) -> Value {
        json!({
            "key": key, "text": format!("thread {key}"), "state": state, "rank": rank,
            "waiting": waiting, "hidden": null, "times": 2, "first_seen": 1000., "last_seen": 2000.,
            "detail": ["a", "b", "c", "d"], "came_from": "sess-bef",
        })
    }

    #[test]
    fn test_no_threads_no_section() {
        assert!(lost_sections(&json!({"lost": []}), 3000., 3).is_empty());
        let aside = json!({"lost": [{"key": "k", "hidden": "drop", "rank": 0}]});
        assert!(lost_sections(&aside, 3000., 3).is_empty());
        assert!(left_over(&aside, 3000.).is_none());
    }

    #[test]
    fn test_the_section_shows_the_top_threads_and_counts_the_rest() {
        let s = json!({"lost": [
            thread("aaa", "lost", 0, true),
            thread("bbb", "carried", 1, false),
            thread("ccc", "open", 2, false),
            thread("ddd", "lost", 2, false),
            thread("eee", "lost", 3, false),
        ]});
        let section = &lost_sections(&s, 3000., 3)[0];
        assert_eq!(section["right"], "4 quiet · f all 5");
        assert_eq!(section["brief"], "4 quiet");
        assert_eq!(section["waits"], true);
        assert_eq!(section["at"], 2000.);
        let items = section["items"].as_array().unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0]["top"], true);
        assert_eq!(items[1]["top"], false);
        assert_eq!(items[0]["flag"], "waiting on you");
        assert_eq!(
            items[0]["meta"],
            format!("quiet {} · 2×", super::super::pages::stamp(2000., 3000.))
        );
        assert_eq!(items[1]["meta"], "from sess-bef · 2×");
        assert_eq!(items[2]["meta"], "open · 2×");
        assert_eq!(items[0]["key"], "lost:aaa");
        assert_eq!(items[0]["lost"], "aaa");
        let detail = items[0]["detail"].as_array().unwrap();
        assert_eq!(detail.len(), 4);
        assert!(detail[0]
            .as_str()
            .unwrap()
            .ends_with("carried through 2 compactions."));
        // 5 items on a phone's tall screen are 6 at most; here 4 qualify
        assert_eq!(
            lost_sections(&s, 3000., 3 * PHONE_MORE)[0]["items"]
                .as_array()
                .unwrap()
                .len(),
            4
        );
    }

    #[test]
    fn test_a_failed_or_unlinked_handoff_is_the_flag_before_waiting() {
        let mut failed = thread("f", "lost", 1, true);
        failed["failed_at"] = json!(1500.);
        failed["why"] = json!("handed to a subagent, which failed");
        let mut unlinked = thread("u", "lost", 1, true);
        unlinked["unlinked"] = json!(true);
        let s = json!({"lost": [failed, unlinked]});
        let items = &lost_sections(&s, 3000., 3)[0]["items"];
        assert_eq!(items[0]["flag"], "its subagent failed");
        assert_eq!(items[0]["detail"][0], "handed to a subagent, which failed");
        assert_eq!(items[1]["flag"], "handoff not linked");
    }

    #[test]
    fn test_left_over_says_how_many_wait() {
        let s =
            json!({"lost": [thread("a", "carried", 0, true), thread("b", "carried", 1, false)]});
        let section = left_over(&s, 3000.).unwrap();
        assert_eq!(section["title"], "Left over");
        assert_eq!(section["right"], "1 waiting · f all 2");
    }

    #[test]
    fn test_only_the_top_thread_or_an_opened_one_shows_its_answers() {
        let s = json!({"lost": [thread("a", "lost", 0, true), thread("b", "lost", 1, false)]});
        let items = lost_sections(&s, 3000., 3)[0]["items"].clone();
        let (first, second) = (&items[0], &items[1]);
        let blocks = answer_blocks(first, 100, 6, &Value::Null, false);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].1["key"], "lost:a:side");
        assert_eq!(blocks[1].1["key"], "lost:a:drop");
        assert!(answer_blocks(second, 100, 6, &Value::Null, false).is_empty());
        assert_eq!(answer_blocks(second, 100, 6, &Value::Null, true).len(), 2);
        // after a send the asks' row says where the words went; drop and later stay
        let sent = json!(["lost:a", true, 3000.]);
        let blocks = answer_blocks(first, 100, 6, &sent, false);
        assert_eq!(blocks.len(), 2);
        assert!(blocks[0].1.is_null());
        assert_eq!(blocks[1].1["key"], "lost:a:drop");
    }
}
