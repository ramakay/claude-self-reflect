//! Recap evidence read from a transcript at Stop time and carried on the
//! episode, so SessionStart composes the recap without opening the transcript.
//!
//! Everything here is a plain reading of rows Claude Code wrote: a failed
//! verification command and its exit code, a background task that was started
//! and never reported finished. Nothing is inferred and nothing is invented:
//! each item carries what a reader needs to find it in the transcript, and a
//! reading that cannot be backed by a receipt is dropped.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// The evidence the recap composer reads beyond the task segments.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecapExtras {
    /// A test, build or lint of the latest task that failed and was not run
    /// again to pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_failure: Option<GateFailure>,
    /// Background tasks started in the main session that no notification, stop
    /// or output read ever closed, newest first, at most [`MAX_BACKGROUND`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub background_open: Vec<BackgroundTask>,
    /// Total background tasks not closed (may exceed `background_open.len()`).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub background_open_total: usize,
    /// More tasks may be open than `background_open_total` counts: some were
    /// dropped for room, so the total is a lower bound.
    #[serde(default, skip_serializing_if = "is_false")]
    pub background_open_more: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl RecapExtras {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// A verification command that failed, with the exit code its result printed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateFailure {
    /// The command as the failure key names it: program and the arguments that
    /// select what it checks.
    pub command: String,
    pub exit_code: i32,
}

/// A background task whose end the transcript never shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundTask {
    /// The id the harness gave it (`<task-id>` in its notification).
    pub id: String,
    /// The call's own description, else the head of its command.
    pub label: String,
}

/// Most background tasks stored per episode.
pub const MAX_BACKGROUND: usize = 3;

/// Longest command a gate failure may name; a longer one abstains rather than
/// being cut (a cut command is not the command that failed).
const GATE_COMMAND_MAX_CHARS: usize = 90;

/// Longest background label kept.
const BACKGROUND_LABEL_MAX_CHARS: usize = 60;

static EXIT_CODE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*Exit code (-?\d+)").unwrap());
static BASH_TASK_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"running in background with ID: ([A-Za-z0-9]+)").unwrap());
static AGENT_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"agentId: ([A-Za-z0-9]+)").unwrap());
static NOTIFICATION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<task-id>([A-Za-z0-9]+)</task-id>").unwrap());

/// The exit code a failed Bash result prints on its first line.
pub(crate) fn exit_code(result_text: &str) -> Option<i32> {
    EXIT_CODE_RE
        .captures(result_text)
        .and_then(|caps| caps[1].parse().ok())
}

/// The command a verification failure key names (`verify:cargo test @/dir` ->
/// `cargo test`), or `None` when it is too long to quote whole or is CSR's own
/// text.
pub(crate) fn gate_command(key: &str) -> Option<String> {
    let body = key.strip_prefix("verify:")?;
    let command = body.rsplit_once(" @").map_or(body, |(command, _)| command);
    let command = command.split_whitespace().collect::<Vec<_>>().join(" ");
    (!command.is_empty()
        && command.chars().count() <= GATE_COMMAND_MAX_CHARS
        && !crate::import::contamination::has_csr_text(&command))
    .then_some(command)
}

/// Most launches the tracker holds at once.
/// A session that starts more background tasks than this without any of them
/// finishing keeps the newest; the count then covers only those tracked.
const MAX_TRACKED: usize = 512;

static FINAL_STATUS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)<status>\s*(?:completed|failed|killed|stopped)\s*</status>").unwrap()
});
/// Whether a stop call's result says it stopped task `task`:
/// `Successfully stopped task: <task>` (or `killed shell`), the id whole.
fn stop_result_names(result: &str, task: &str) -> bool {
    Regex::new(&format!(
        r"(?i)\bsuccessfully\s+(?:stopped|killed)\s+(?:task|shell):\s*{}\b",
        regex::escape(task)
    ))
    .is_ok_and(|re| re.is_match(result))
}

/// One notification envelope, whole.
static ENVELOPE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<task-notification>(.*?)</task-notification>").unwrap());
/// The free-text parts of an envelope, which are not its status.
static ENVELOPE_BODY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)<(?:summary|output|note|result)>.*?(?:</(?:summary|output|note|result)>|$)")
        .unwrap()
});

/// Tracks background launches over main-session rows. A task closes in two
/// ways only: the harness's own `<task-notification>` for it with a terminal
/// status, or the result of a `TaskStop` / `KillShell` call whose input names
/// exactly that task and whose result says `Successfully stopped task: <id>`.
/// Nothing else closes one: a poll (`TaskOutput` / `BashOutput`), a call that
/// merely names the id, or any other result text. State is indexed by task id
/// and bounded by [`MAX_TRACKED`].
#[derive(Default)]
pub(crate) struct BackgroundTracker {
    /// `tool_use` id -> (launch order, label), for launches whose result has
    /// not arrived.
    launching: HashMap<String, (u64, String)>,
    /// Launched tasks nothing has closed: task id -> (launch order, label).
    open: HashMap<String, (u64, String)>,
    /// `tool_use` id of a stop call -> the open task id its input names.
    stopping: HashMap<String, (u64, String)>,
    /// Next launch order.
    seq: u64,
    /// Open tasks dropped for room: the tracked count is then a lower bound.
    evicted: usize,
    /// A stop that could not be tracked for room: nothing is claimed open.
    unreliable: bool,
}

/// Drops the oldest entry of `map` while it holds more than [`MAX_TRACKED`],
/// and says how many it dropped.
fn bound<V>(map: &mut HashMap<String, (u64, V)>) -> usize {
    let mut dropped = 0;
    while map.len() > MAX_TRACKED {
        let Some(oldest) = map
            .iter()
            .min_by_key(|(_, (order, _))| *order)
            .map(|(key, _)| key.clone())
        else {
            return dropped;
        };
        map.remove(&oldest);
        dropped += 1;
    }
    dropped
}

impl BackgroundTracker {
    /// A `tool_use` block of the main session.
    pub(crate) fn tool_use(&mut self, block: &serde_json::Value) {
        let Some(name) = block.get("name").and_then(|v| v.as_str()) else {
            return;
        };
        let Some(input) = block.get("input") else {
            return;
        };
        let id = block.get("id").and_then(|v| v.as_str());
        let background = input
            .get("run_in_background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if background && matches!(name, "Bash" | "Agent" | "Task") {
            if let Some(id) = id {
                let label = input
                    .get("description")
                    .and_then(|v| v.as_str())
                    .filter(|d| !d.trim().is_empty())
                    .or_else(|| input.get("command").and_then(|v| v.as_str()))
                    .map(|text| text.lines().next().unwrap_or("").trim().to_string())
                    .unwrap_or_default();
                self.seq += 1;
                self.launching.insert(id.to_string(), (self.seq, label));
                self.evicted += bound(&mut self.launching);
            }
            return;
        }
        if !matches!(name, "TaskStop" | "KillShell") {
            return;
        }
        // The one task the call's own `task_id` / `shell_id` field names.
        let task = ["task_id", "shell_id"]
            .iter()
            .filter_map(|field| input.get(*field).and_then(|v| v.as_str()))
            .find(|task| self.open.contains_key(*task));
        let (Some(call_id), Some(task)) = (id, task) else {
            return;
        };
        if self.stopping.len() >= MAX_TRACKED {
            self.unreliable = true;
            return;
        }
        self.seq += 1;
        self.stopping
            .insert(call_id.to_string(), (self.seq, task.to_string()));
    }

    /// A `tool_result` block answering `tool_use_id`.
    pub(crate) fn tool_result(&mut self, tool_use_id: &str, text: &str, is_error: bool) {
        if let Some((_, task)) = self.stopping.remove(tool_use_id) {
            if !is_error && stop_result_names(text, &task) {
                self.open.remove(&task);
            }
            return;
        }
        let Some((order, label)) = self.launching.remove(tool_use_id) else {
            return;
        };
        if is_error {
            return;
        }
        let task = BASH_TASK_ID_RE
            .captures(text)
            .or_else(|| AGENT_ID_RE.captures(text))
            .map(|caps| caps[1].to_string());
        if let Some(task) = task {
            self.open.insert(task, (order, label));
            self.evicted += bound(&mut self.open);
        }
    }

    /// The text of a harness-injected row. It closes the tasks of the
    /// notifications it consists of and nothing else: the text must be nothing
    /// but whole `<task-notification>` envelopes (a user quoting one, with
    /// words around it, is not the harness), and an envelope closes its task
    /// only with a terminal status read outside its summary, output and note.
    pub(crate) fn notification(&mut self, text: &str) {
        let text = text.trim();
        if !text.starts_with("<task-notification>") {
            return;
        }
        if !ENVELOPE_RE.replace_all(text, "").trim().is_empty() {
            return;
        }
        for envelope in ENVELOPE_RE.captures_iter(text) {
            // One status tag in the whole envelope: a second one is text a
            // task printed, and which of the two is the harness's is unknown.
            if envelope[1].to_ascii_lowercase().matches("<status>").count() != 1 {
                continue;
            }
            let head = ENVELOPE_BODY_RE.replace_all(&envelope[1], "");
            let Some(task) = NOTIFICATION_RE.captures(&head) else {
                continue;
            };
            if FINAL_STATUS_RE.is_match(&head) {
                self.open.remove(&task[1]);
            }
        }
    }

    /// Whether the tracked count is a lower bound: tasks were dropped for room.
    pub(crate) fn more(&self) -> bool {
        self.evicted > 0
    }

    /// `(open tasks newest first, at most MAX_BACKGROUND; total open tracked)`.
    pub(crate) fn open(&self) -> (Vec<BackgroundTask>, usize) {
        if self.unreliable {
            return (Vec::new(), 0);
        }
        let mut open: Vec<(&String, &(u64, String))> = self.open.iter().collect();
        open.sort_by_key(|(_, (order, _))| std::cmp::Reverse(*order));
        let total = open.len();
        let shown = open
            .into_iter()
            .filter_map(|(task, (_, label))| {
                let label = label.split_whitespace().collect::<Vec<_>>().join(" ");
                let label = if label.chars().count() > BACKGROUND_LABEL_MAX_CHARS {
                    let cut: String = label.chars().take(BACKGROUND_LABEL_MAX_CHARS).collect();
                    cut.rsplit_once(' ')
                        .map_or(cut.clone(), |(head, _)| head.to_string())
                } else {
                    label
                };
                (crate::hooks::recap::has_substance(&label)
                    && !crate::import::contamination::has_csr_text(&label))
                .then(|| BackgroundTask {
                    id: task.clone(),
                    label,
                })
            })
            .take(MAX_BACKGROUND)
            .collect();
        (shown, total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_code_reads_the_first_line_only() {
        assert_eq!(exit_code("Exit code 101\nerror: boom"), Some(101));
        assert_eq!(exit_code("ok\nExit code 2"), None);
        assert_eq!(exit_code("File not found"), None);
    }

    #[test]
    fn gate_command_drops_the_directory_and_abstains_when_long() {
        assert_eq!(
            gate_command("verify:cargo test --release --lib @/Users/x/repo").as_deref(),
            Some("cargo test --release --lib")
        );
        let long = format!("verify:cargo test {} @/x", "--features a".repeat(12));
        assert_eq!(gate_command(&long), None);
        assert_eq!(gate_command("cargo test"), None);
    }

    fn launch(tracker: &mut BackgroundTracker, id: &str, label: &str, task: &str) {
        tracker.tool_use(&serde_json::json!({
            "type": "tool_use", "id": id, "name": "Bash",
            "input": {"command": "sleep 100", "description": label, "run_in_background": true}
        }));
        tracker.tool_result(
            id,
            &format!(
                "Command running in background with ID: {task}. Output is being written to: /tmp/x"
            ),
            false,
        );
    }

    #[test]
    fn a_launch_with_no_notification_is_open_and_a_notification_closes_it() {
        let mut tracker = BackgroundTracker::default();
        launch(&mut tracker, "t1", "render reel", "bq1");
        launch(&mut tracker, "t2", "upload build", "bq2");
        assert_eq!(tracker.open().1, 2);
        tracker.notification(
            "<task-notification>\n<task-id>bq1</task-id>\n<status>completed</status>\n</task-notification>",
        );
        let (open, total) = tracker.open();
        assert_eq!(total, 1);
        assert_eq!(open[0].id, "bq2");
        assert_eq!(open[0].label, "upload build");
    }

    fn call(tracker: &mut BackgroundTracker, id: &str, name: &str, input: serde_json::Value) {
        tracker.tool_use(&serde_json::json!({
            "type": "tool_use", "id": id, "name": name, "input": input
        }));
    }

    fn done(task: &str, status: &str) -> String {
        format!(
            "<task-notification>\n<task-id>{task}</task-id>\n<status>{status}</status>\n</task-notification>"
        )
    }

    #[test]
    fn only_a_stop_call_that_names_the_task_and_whose_result_says_so_closes_it() {
        let mut tracker = BackgroundTracker::default();
        launch(&mut tracker, "t1", "render reel", "bq1");
        launch(&mut tracker, "t2", "upload build", "bq2");
        call(
            &mut tracker,
            "x",
            "TaskStop",
            serde_json::json!({"task_id": "bq1"}),
        );
        // The call alone closes nothing.
        assert_eq!(tracker.open().1, 2);
        // A result about another task, or a failed call, closes nothing.
        tracker.tool_result(
            "x",
            r#"{"message":"Successfully stopped task: bq2 (x)"}"#,
            false,
        );
        assert_eq!(tracker.open().1, 2);
        call(
            &mut tracker,
            "y",
            "TaskStop",
            serde_json::json!({"task_id": "bq1"}),
        );
        tracker.tool_result("y", "No task found with ID: bq1", true);
        assert_eq!(tracker.open().1, 2);
        call(
            &mut tracker,
            "z",
            "TaskStop",
            serde_json::json!({"task_id": "bq1"}),
        );
        tracker.tool_result(
            "z",
            r#"{"message":"Successfully stopped task: bq1 (sleep 100)"}"#,
            false,
        );
        let (open, total) = tracker.open();
        assert_eq!(total, 1);
        assert_eq!(open[0].id, "bq2");
    }

    #[test]
    fn no_tool_call_or_result_closes_a_task() {
        let mut tracker = BackgroundTracker::default();
        launch(&mut tracker, "t1", "render reel", "bq1");
        launch(&mut tracker, "t2", "upload build", "bq2");
        for (id, name) in [("c", "TaskOutput"), ("d", "BashOutput")] {
            call(
                &mut tracker,
                id,
                name,
                serde_json::json!({"task_id": "bq1"}),
            );
        }
        for id in ["c", "d"] {
            tracker.tool_result(
                id,
                "Successfully stopped task: bq1\n<status>completed</status>",
                false,
            );
        }
        // Not even a result that prints a notification envelope.
        tracker.tool_result("t9", &done("bq2", "completed"), false);
        assert_eq!(tracker.open().1, 2);
    }

    #[test]
    fn only_a_terminal_status_in_the_envelope_closes_a_task() {
        for status in ["running", "pending", "started", ""] {
            let mut tracker = BackgroundTracker::default();
            launch(&mut tracker, "t1", "render reel", "bq1");
            tracker.notification(&done("bq1", status));
            assert_eq!(tracker.open().1, 1, "status {status:?}");
        }
        for status in ["completed", "failed", "killed", "stopped"] {
            let mut tracker = BackgroundTracker::default();
            launch(&mut tracker, "t1", "render reel", "bq1");
            tracker.notification(&done("bq1", status));
            assert_eq!(tracker.open().1, 0, "status {status:?}");
        }
        // A status printed inside the task's own output is not the envelope's.
        let mut tracker = BackgroundTracker::default();
        launch(&mut tracker, "t1", "render reel", "bq1");
        tracker.notification(
            "<task-notification><task-id>bq1</task-id><status>running</status><output>Example: <status>completed</status></output></task-notification>",
        );
        tracker.notification(
            "<task-notification><task-id>bq1</task-id><output></output><status>completed</status>",
        );
        assert_eq!(tracker.open().1, 1);
        // Words around an envelope make it a quotation, not the harness's.
        tracker.notification(&format!("Explain this: {}", done("bq1", "completed")));
        assert_eq!(tracker.open().1, 1);
    }

    #[test]
    fn tracking_is_bounded_and_linear_in_the_rows() {
        let mut tracker = BackgroundTracker::default();
        let started = std::time::Instant::now();
        for n in 0..10_000 {
            launch(
                &mut tracker,
                &format!("t{n}"),
                "render a reel",
                &format!("b{n}x"),
            );
            call(
                &mut tracker,
                &format!("r{n}"),
                "Read",
                serde_json::json!({"file_path": "/x/y.rs"}),
            );
        }
        let (open, total) = tracker.open();
        assert!(total <= MAX_TRACKED, "{total} tasks tracked");
        assert!(tracker.more());
        assert_eq!(open.len(), MAX_BACKGROUND);
        // The newest are the ones kept.
        assert_eq!(open[0].id, "b9999x");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_launch_that_errored_or_never_answered_is_not_a_task() {
        let mut tracker = BackgroundTracker::default();
        tracker.tool_use(&serde_json::json!({
            "type": "tool_use", "id": "t1", "name": "Bash",
            "input": {"command": "x", "run_in_background": true}
        }));
        assert_eq!(tracker.open().1, 0);
        tracker.tool_result("t1", "Command running in background with ID: bq1", true);
        assert_eq!(tracker.open().1, 0);
    }

    #[test]
    fn an_agent_launch_is_tracked_by_its_agent_id() {
        let mut tracker = BackgroundTracker::default();
        tracker.tool_use(&serde_json::json!({
            "type": "tool_use", "id": "a1", "name": "Agent",
            "input": {"description": "Study reference reel", "prompt": "p", "run_in_background": true}
        }));
        tracker.tool_result(
            "a1",
            "Async agent launched successfully.\nagentId: a4c996de2463be3d6 (internal)",
            false,
        );
        let (open, _) = tracker.open();
        assert_eq!(open[0].id, "a4c996de2463be3d6");
        tracker.notification(&done("a4c996de2463be3d6", "completed"));
        assert_eq!(tracker.open().1, 0);
    }
}
