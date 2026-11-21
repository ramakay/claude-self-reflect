//! Prep-compact: before a compaction, the threads you set aside for later ("later" in the pane's
//! Lost and found) are handed to the summarizer as reminders, in a section of their own. Never
//! into Pending Tasks: that list is how Lost and found sees a thread go quiet, and Claude reads
//! it as its to-do list. Port of the removed `carry.ts` and of the `carry` hook.
//!
//! The hook is a Gate on `session.compact` that rewrites `instructions` and nothing else. A
//! failure, or a source that takes over two seconds, lets the compaction run as it was. Lengths
//! are the TypeScript's: UTF-16 code units.

use crate::companion::{
    lost,
    paths::Paths,
    relay::{
        dispatch::{handler, Reply, Table},
        host::BoxFuture,
    },
};
use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;
use std::{sync::Arc, time::Duration};

/// What carries a thread of Lost and found to the summarizer: lenient, as the TypeScript reads it.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Thread {
    pub text: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub waiting: bool,
    #[serde(default)]
    pub hidden: Option<String>,
    #[serde(default)]
    pub later: bool,
    #[serde(default)]
    pub rank: Option<usize>,
}

/// How many threads Lost and found shows waiting on you: not set aside, lost or open (rank under
/// 3), and waiting. The pane's `lost_waiting`, by which the compaction card counts them.
pub fn waiting_on(threads: &[Thread]) -> usize {
    threads
        .iter()
        .filter(|t| t.hidden.as_deref().is_none_or(str::is_empty))
        .filter(|t| t.rank.unwrap_or(9) < 3 && t.waiting)
        .count()
}

const HEAD: &str = "After the summary, add a section headed 'Reminders from CSR (not tasks)' and \
    list these lines verbatim. The person set them aside for later; do not act on them unless the \
    person asks:";
pub const CARRY_MAX: usize = 8;
pub const CARRY_CHARS: usize = 1200;
/// Past this the source is not waited for: the compaction goes on as it was.
pub const LOST_LIMIT: Duration = Duration::from_secs(2);

fn width(text: &str) -> usize {
    text.encode_utf16().count()
}

/// The first `units` UTF-16 units of `text`; a character that would be cut in two is left out.
fn prefix(text: &str, units: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        used += c.len_utf16();
        if used > units {
            break;
        }
        out.push(c);
    }
    out
}

/// The block added to the compaction's instructions: up to eight threads, the whole of it within
/// 1200 characters; none when there is nothing to carry.
pub fn carry_block(threads: &[Thread]) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut room = CARRY_CHARS as i64 - width(HEAD) as i64;
    for thread in threads {
        if lines.len() >= CARRY_MAX || thread.hidden.as_deref() == Some("drop") || !thread.later {
            continue;
        }
        let words: Vec<&str> = thread.text.split_whitespace().collect();
        let line = format!("- {}", words.join(" "));
        if room < 40 {
            break; // no room for a line worth reading
        }
        let fitted = if width(&line) as i64 + 1 > room {
            format!("{}…", prefix(&line, (room - 2) as usize))
        } else {
            line
        };
        room -= width(&fitted) as i64 + 1;
        lines.push(fitted);
    }
    if lines.is_empty() {
        return None;
    }
    Some(format!("{HEAD}\n{}", lines.join("\n")))
}

/// The threads in what the source gave; none when it is not a list of them.
pub fn threads_of(value: &Value) -> Option<Vec<Thread>> {
    serde_json::from_value(value.clone()).ok()
}

/// Where the threads come from: `(cwd, sid)`, answered with the pane's `--lost` JSON.
pub type Threads = Arc<dyn Fn(String, String) -> BoxFuture<Result<Value>> + Send + Sync>;

/// The pane's `--lost` mode, run in this process: Lost and found for the session, as JSON.
pub fn in_process() -> Threads {
    Arc::new(|_cwd, sid| {
        Box::pin(async move {
            let found =
                tokio::task::spawn_blocking(move || lost::lost_json(&Paths::from_env(), &sid))
                    .await?;
            Ok(serde_json::to_value(found)?)
        })
    })
}

/// The `carry` entry, with the source of threads it uses.
pub fn register_with(t: &mut Table, threads: Threads) {
    t.on(
        "carry",
        "carry",
        handler(move |cx, e| {
            let threads = threads.clone();
            async move {
                // A subagent's compaction is the subagent's own; its threads are not the session's.
                if e.get("agentId").is_some_and(|id| !id.is_null()) {
                    return Ok(Reply::none());
                }
                let asked = threads(cx.cwd.clone(), cx.sid.clone());
                let block = match tokio::time::timeout(LOST_LIMIT, asked).await {
                    Ok(Ok(found)) => threads_of(&found).and_then(|found| carry_block(&found)),
                    _ => None,
                };
                let Some(block) = block else {
                    return Ok(Reply::none());
                };
                let mut next = e.clone();
                next["instructions"] = match e["instructions"].as_str() {
                    Some(had) if !had.is_empty() => Value::String(format!("{had}\n\n{block}")),
                    _ => Value::String(block),
                };
                Ok(Reply::next(next))
            }
        }),
    );
}

pub fn register(t: &mut Table) {
    register_with(t, in_process());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::companion::relay::{
        dispatch::testing::{fixture_in, run, Fixture},
        host::fake::World,
    };
    use serde_json::json;
    use std::sync::Mutex;

    const REMINDERS: &str =
        "After the summary, add a section headed 'Reminders from CSR (not tasks)'";
    type Asks = Arc<Mutex<Vec<(String, String)>>>;

    fn threads() -> Value {
        json!([
            { "text": "Awaiting the user’s go on the push guard.", "state": "lost",
                "waiting": true },
            { "text": "Fix the importer: remove the old cache.", "state": "open",
                "waiting": false },
            { "text": "Try the cache countdown in the masthead.", "state": "lost",
                "waiting": false, "later": true },
        ])
    }

    /// A source that answers `answer` and notes `(cwd, sid)` of each ask.
    fn source(answer: Result<Value, String>) -> (Threads, Asks) {
        let asks: Asks = Arc::new(Mutex::new(Vec::new()));
        let noted = asks.clone();
        let threads: Threads = Arc::new(move |cwd, sid| {
            noted.lock().unwrap().push((cwd, sid));
            let answer = answer.clone();
            Box::pin(async move { answer.map_err(|why| anyhow::anyhow!(why)) })
        });
        (threads, asks)
    }

    fn hangs() -> Threads {
        Arc::new(|_, _| Box::pin(std::future::pending()))
    }

    fn table(threads: Threads) -> Table {
        let mut table = Table::new();
        register_with(&mut table, threads);
        table
    }

    fn session() -> Fixture {
        fixture_in(World::default(), "sess-1")
    }

    /// What the compaction is told: the rewritten instructions, or the ones it had.
    async fn told(table: &Table, fx: &Fixture, e: Value) -> Option<String> {
        let had = e["instructions"].as_str().map(str::to_string);
        let reply = run(table, "carry", &fx.cx, e).await;
        match reply.next {
            Some(next) => next["instructions"].as_str().map(str::to_string),
            None => had,
        }
    }

    fn compact(trigger: &str, instructions: Option<&str>) -> Value {
        let mut e = json!({ "trigger": trigger, "messages": [] });
        if let Some(text) = instructions {
            e["instructions"] = json!(text);
        }
        e
    }

    #[tokio::test]
    async fn prep_compact_hands_the_summary_only_what_you_set_aside_for_later_as_reminders() {
        let (found, asks) = source(Ok(threads()));
        let fx = session();
        let said = told(&table(found), &fx, compact("manual", Some("keep the plan")))
            .await
            .unwrap();
        assert!(said.starts_with(&format!(
            "keep the plan\n\n{}",
            &REMINDERS[..REMINDERS.len() - 1]
        )));
        assert!(said.contains("- Try the cache countdown in the masthead."));
        // Open and lost threads stay out: Lost and found must see them drop.
        assert!(!said.contains("push guard"));
        assert!(!said.contains("Fix the importer"));
        assert!(!said.contains("Pending Tasks"));
        assert_eq!(
            *asks.lock().unwrap(),
            [("/repo".to_string(), "sess-1".to_string())]
        );
    }

    #[tokio::test]
    async fn prep_compact_holds_the_block_to_eight_threads_and_1200_characters() {
        let many: Vec<Value> = (0..20)
            .map(|k| {
                json!({ "text": format!("Thread {k} {}", "x".repeat(200)), "state": "lost",
                    "waiting": false, "later": true })
            })
            .collect();
        let (found, _) = source(Ok(json!(many)));
        let said = told(&table(found), &session(), compact("auto", None))
            .await
            .unwrap();
        assert!(width(&said) <= 1200, "{}", width(&said));
        assert!(said.matches('\n').count() <= 9);
        assert!(said.contains("Thread 0 "));
        // Eight at most, whatever the room: short threads stop at eight.
        let short: Vec<Value> = (0..20)
            .map(|k| {
                json!({ "text": format!("Short {k}: a thread worth reading in a summary"),
                "later": true })
            })
            .collect();
        let block = carry_block(&threads_of(&json!(short)).unwrap()).unwrap();
        assert_eq!(
            block.lines().filter(|line| line.starts_with("- ")).count(),
            CARRY_MAX
        );
        assert!(block.contains("Short 7:") && !block.contains("Short 8:"));
    }

    #[tokio::test]
    async fn prep_compact_fails_open_a_pane_py_that_fails_or_has_no_threads_leaves_the() {
        let (failing, _) = source(Err("exit 2".into()));
        let table = table(failing);
        let fx = session();
        let kept = told(&table, &fx, compact("manual", Some("keep the plan"))).await;
        assert_eq!(kept.as_deref(), Some("keep the plan"));
        assert_eq!(told(&table, &fx, compact("manual", None)).await, None);
    }

    #[tokio::test]
    async fn prep_compact_leaves_a_compaction_with_no_threads_to_carry_unchanged() {
        let (found, _) = source(Ok(json!([])));
        let kept = told(
            &table(found),
            &session(),
            compact("manual", Some("keep the plan")),
        )
        .await;
        assert_eq!(kept.as_deref(), Some("keep the plan"));
    }

    #[tokio::test(start_paused = true)]
    async fn prep_compact_fails_open_on_a_pane_py_that_times_out_or_prints_no_json_and_passes() {
        let fx = session();
        let started = tokio::time::Instant::now();
        let kept = told(
            &table(hangs()),
            &fx,
            compact("manual", Some("keep the plan")),
        )
        .await;
        assert_eq!(kept.as_deref(), Some("keep the plan"));
        assert_eq!(
            started.elapsed(),
            LOST_LIMIT,
            "it waited its two seconds and not longer"
        );
        // A source that answers within the limit is not cut short.
        let (found, _) = source(Ok(threads()));
        let said = told(&table(found), &fx, compact("manual", None))
            .await
            .unwrap();
        assert!(said.starts_with(&REMINDERS[..REMINDERS.len() - 1]));
    }

    #[tokio::test]
    async fn prep_compact_leaves_a_compaction_unchanged_when_pane_py_prints_something_that_is() {
        let (found, asks) = source(Ok(json!("Traceback (most recent call last)")));
        let fx = session();
        assert_eq!(told(&table(found), &fx, compact("auto", None)).await, None);
        assert_eq!(asks.lock().unwrap().len(), 1);
        // A list of things that are not threads is no list of threads either.
        let (found, _) = source(Ok(json!([1, 2])));
        assert_eq!(told(&table(found), &fx, compact("auto", None)).await, None);
    }

    #[tokio::test]
    async fn prep_compact_also_carries_the_threads_into_a_compaction_made_ahead_of_time() {
        let (found, _) = source(Ok(threads()));
        let said = told(&table(found), &session(), compact("precompute", None))
            .await
            .unwrap();
        assert!(said.contains("Reminders from CSR (not tasks)"));
    }

    #[tokio::test]
    async fn a_main_session_compaction_gets_the_instructions_it_had_and_the_lost_and_found() {
        let (found, _) = source(Ok(threads()));
        let said = told(
            &table(found),
            &session(),
            compact("manual", Some("keep the plan")),
        )
        .await
        .unwrap();
        // Exactly what prep-compact makes of it.
        assert!(said.starts_with(&format!(
            "keep the plan\n\n{}",
            &REMINDERS[..REMINDERS.len() - 1]
        )));
        assert!(said.contains("- Try the cache countdown in the masthead."));
        assert!(!said.contains("CSR COMPACT KEEP") && !said.contains("Task brief"));
    }

    #[tokio::test]
    async fn a_main_session_compaction_with_no_threads_to_carry_keeps_exactly_its() {
        let (found, _) = source(Ok(json!([])));
        let table = table(found);
        let fx = session();
        let kept = told(&table, &fx, compact("manual", Some("keep the plan"))).await;
        assert_eq!(kept.as_deref(), Some("keep the plan"));
        assert_eq!(told(&table, &fx, compact("auto", None)).await, None);
    }

    #[tokio::test]
    async fn csr_s_own_compaction_carries_the_threads_set_aside_for_later_after_its_keep_list() {
        let lost = json!([
            { "text": "Wait for agent ab763f58e5be44c02's timeout-fix report.", "state": "lost",
                "waiting": true, "hidden": null, "later": false },
            { "text": "Try the cache countdown in the masthead.", "state": "lost",
                "waiting": false, "hidden": "later", "later": true },
        ]);
        let (found, asks) = source(Ok(lost));
        // The compactor's keep list is its package's; any keep list opens with its own header.
        let keep = "CSR COMPACT KEEP — running work:\n- a427c126 (subagent) Explore CapCut";
        let mut e = compact("plugin", Some(keep));
        e["messages"] = json!([{ "role": "user", "text": "one", "toolUses": [] }]);
        let said = told(&table(found), &session(), e).await.unwrap();
        assert!(
            said.starts_with(&format!("{keep}\n\n")),
            "the keep list first"
        );
        assert!(said.contains("add a section headed 'Reminders from CSR (not tasks)'"));
        assert!(said.contains("- Try the cache countdown in the masthead."));
        // Waiting, not set aside: Lost and found must see it go quiet.
        assert!(!said.contains("ab763f58e5be44c02"));
        assert_eq!(
            *asks.lock().unwrap(),
            [("/repo".to_string(), "sess-1".to_string())]
        );
    }

    // Not in the TypeScript's tests by name: the rest of what the module does.

    #[tokio::test]
    async fn a_subagents_compaction_is_not_asked_the_sessions_threads() {
        let (found, asks) = source(Ok(threads()));
        let e = json!({ "trigger": "auto", "instructions": "keep the plan", "agentId": "sub-1" });
        let kept = told(&table(found), &session(), e).await;
        assert_eq!(kept.as_deref(), Some("keep the plan"));
        assert!(asks.lock().unwrap().is_empty());
    }

    #[test]
    fn a_thread_waits_on_you_when_not_set_aside_open_or_lost_and_waiting() {
        let all = threads_of(&json!([
            { "text": "a", "waiting": true, "rank": 0 },
            { "text": "b", "waiting": true, "rank": 2 },
            { "text": "c", "waiting": true, "rank": 3 },
            { "text": "d", "waiting": false, "rank": 0 },
            { "text": "e", "waiting": true, "rank": 0, "hidden": "later" },
            { "text": "f", "waiting": true, "rank": 1, "hidden": "" },
            { "text": "g", "waiting": true },
        ]))
        .unwrap();
        assert_eq!(
            waiting_on(&all),
            3,
            "a, b and f; no rank counts as the last"
        );
    }

    #[test]
    fn a_thread_dropped_or_not_for_later_is_not_carried_and_words_are_one_line() {
        let all = threads_of(&json!([
            { "text": "dropped", "later": true, "hidden": "drop" },
            { "text": "now", "later": false },
            { "text": "two\n  lines\tof   text, for later", "later": true },
        ]))
        .unwrap();
        let block = carry_block(&all).unwrap();
        assert!(block.ends_with("\n- two lines of text, for later"));
        assert!(!block.contains("dropped") && !block.contains("now"));
        assert_eq!(carry_block(&all[..2]), None);
    }

    #[test]
    fn a_line_that_does_not_fit_is_cut_at_a_whole_character_with_an_ellipsis() {
        // Emoji are two UTF-16 units each: the cut never splits one.
        let text = "🙂".repeat(700);
        let all = threads_of(&json!([{ "text": text, "later": true }])).unwrap();
        let block = carry_block(&all).unwrap();
        assert!(width(&block) <= CARRY_CHARS, "{}", width(&block));
        assert!(block.ends_with('…'));
        assert!(block
            .lines()
            .last()
            .unwrap()
            .chars()
            .filter(|c| *c != '…')
            .all(|c| c == '🙂' || c == '-' || c == ' '));
        assert_eq!(prefix("a🙂b", 2), "a");
        assert_eq!(prefix("a🙂b", 3), "a🙂");
    }

    #[test]
    fn the_threads_the_pane_prints_are_threads_to_carry() {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::new(home.path().to_path_buf());
        let found = serde_json::to_value(lost::lost_json(&paths, "")).unwrap();
        assert!(threads_of(&found).is_some(), "{found}");
        assert!(threads_of(&json!({ "threads": [] })).is_none());
    }
}
