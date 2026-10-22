//! Recap evidence read from a transcript at Stop time and carried on the
//! episode, so SessionStart composes the recap without opening the transcript.
//!
//! Everything here is a plain reading of rows Claude Code wrote: a failed
//! verification command and its exit code, a background task that was started
//! and never reported finished, a line the assistant itself put at the head of
//! its closing message ("Next:", "Still open:"). Nothing is inferred and
//! nothing is invented: each item carries what a reader needs to find it in the
//! transcript, and a reading that cannot be backed by a receipt is dropped.

use std::collections::{HashMap, HashSet};
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
    /// The closing message's "Next:" line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closing_next: Option<ClosingLine>,
    /// The closing message's "Still open:" / "Remaining:" line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closing_open: Option<ClosingLine>,
    /// Total background tasks not closed (may exceed `background_open.len()`).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub background_open_total: usize,
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

/// One line quoted from the assistant's closing message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClosingLine {
    /// The sentence(s) after the marker, whole.
    pub text: String,
    /// First 8 characters of the transcript row's `uuid`: the receipt.
    pub row: String,
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

/// Tracks background launches and what closes them, over main-session rows.
#[derive(Default)]
pub(crate) struct BackgroundTracker {
    /// `tool_use` id -> label, for launches whose result has not arrived.
    launching: HashMap<String, String>,
    /// Launched tasks in launch order: `(task id, label)`.
    launched: Vec<(String, String)>,
    /// Task ids a notification, a stop or an output read closed.
    closed: HashSet<String>,
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
                self.launching.insert(id.to_string(), label);
            }
            return;
        }
        // A stop closes a task; a read of its output file or a poll of its id
        // means the assistant looked at it, so it is not a task nobody saw.
        let closes = matches!(
            name,
            "TaskStop" | "KillShell" | "TaskOutput" | "BashOutput" | "Read" | "Bash"
        );
        if !closes || self.launched.is_empty() {
            return;
        }
        let haystack = match name {
            "Bash" => input
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            _ => input.to_string(),
        };
        for (task, _) in &self.launched {
            if !self.closed.contains(task) && haystack.contains(task.as_str()) {
                self.closed.insert(task.clone());
            }
        }
    }

    /// A `tool_result` block answering `tool_use_id`.
    pub(crate) fn tool_result(&mut self, tool_use_id: &str, text: &str, is_error: bool) {
        let Some(label) = self.launching.remove(tool_use_id) else {
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
            self.launched.push((task, label));
        }
    }

    /// Any row text that may carry a `<task-notification>`.
    pub(crate) fn notification(&mut self, text: &str) {
        if !text.contains("<task-notification>") {
            return;
        }
        for caps in NOTIFICATION_RE.captures_iter(text) {
            self.closed.insert(caps[1].to_string());
        }
    }

    /// `(open tasks newest first, at most MAX_BACKGROUND; total open)`.
    pub(crate) fn open(&self) -> (Vec<BackgroundTask>, usize) {
        let open: Vec<&(String, String)> = self
            .launched
            .iter()
            .filter(|(task, _)| !self.closed.contains(task))
            .collect();
        let total = open.len();
        let shown = open
            .into_iter()
            .rev()
            .filter_map(|(task, label)| {
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

/// Labels that name the next step, longest first.
const NEXT_LABELS: &[&str] = &["next move", "next steps", "next step", "next up", "next"];
/// Labels that name what is still open, longest first.
const OPEN_LABELS: &[&str] = &[
    "still waiting on you",
    "still waiting on",
    "still open",
    "still to do",
    "left to do",
    "remaining",
    "queued",
];

/// Prose openers of a next step that need no colon: `Next is ...`,
/// `Next I'll ...`. The opener's own words stay in the quote when they carry
/// the subject ("I'll recover ...").
const NEXT_PROSE: &[(&str, bool)] = &[
    ("next move is ", true),
    ("next step is ", true),
    ("next up is ", true),
    ("next is ", true),
    ("next i'll ", false),
    ("next i will ", false),
    ("next we'll ", false),
    ("next i\u{2019}ll ", false),
    ("next we\u{2019}ll ", false),
];

/// What a closing-message line says, and which kind it is.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ClosingKind {
    Next,
    Open,
}

/// The last marker line of `text` (the assistant's closing message), as the
/// whole sentence(s) after the marker. A marker counts only at the start of a
/// line (after a bullet and emphasis), outside code fences, blockquotes and
/// indented code, and not as an item of a numbered list (where "Next" only
/// orders the items). `None` when there is no such line or what follows is
/// empty, a question, "none", or CSR's own text.
pub(crate) fn closing_marker(text: &str) -> Option<(ClosingKind, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut fenced = false;
    let mut found: Option<(ClosingKind, String)> = None;
    for (at, raw) in lines.iter().enumerate() {
        let trimmed = raw.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced
            || trimmed.starts_with('>')
            || trimmed.starts_with('|')
            || trimmed.starts_with('#')
        {
            continue;
        }
        let indent = raw.len() - trimmed.len();
        let (body, bulleted, numbered) = strip_list_marker(trimmed);
        if numbered || ((raw.starts_with('\t') || indent >= 4) && !bulleted) {
            continue;
        }
        let body = body.trim_start_matches(['*', '_']).trim_start();
        let lower = body.to_lowercase();
        let Some((kind, content)) = marker_content(body, &lower) else {
            continue;
        };
        let mut content = content.trim().to_string();
        if content.is_empty() {
            content = following_items(&lines[at + 1..], indent);
        }
        if let Some(sentence) = marker_sentence(&content) {
            found = Some((kind, sentence));
        }
    }
    found
}

/// `(rest, bulleted, numbered)` of a line without its list marker.
fn strip_list_marker(line: &str) -> (&str, bool, bool) {
    for marker in ["- ", "* ", "+ ", "\u{2022} "] {
        if let Some(rest) = line.strip_prefix(marker) {
            return (rest.trim_start(), true, false);
        }
    }
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if (1..=3).contains(&digits) {
        let after = &line[digits..];
        if after.starts_with(". ") || after.starts_with(") ") {
            return (line, false, true);
        }
    }
    (line, false, false)
}

/// The marker a line opens with and what follows it on that line.
fn marker_content<'a>(body: &'a str, lower: &str) -> Option<(ClosingKind, &'a str)> {
    // `lower` is the lowercase of `body`; byte offsets agree for the ASCII
    // markers matched below, and a non-ASCII start never matches them.
    if !body.is_char_boundary(0) || lower.len() != body.len() {
        return None;
    }
    for (label, kind) in NEXT_LABELS
        .iter()
        .map(|l| (*l, ClosingKind::Next))
        .chain(OPEN_LABELS.iter().map(|l| (*l, ClosingKind::Open)))
    {
        if !lower.starts_with(label) {
            continue;
        }
        let Some(rest) = body.get(label.len()..) else {
            continue;
        };
        if rest.chars().next().is_some_and(char::is_alphanumeric) {
            continue;
        }
        // `Next:`, `**Next:**`, `**Next**:`, `Next move -`.
        let rest = rest.trim_start_matches(['*', '_']);
        let rest_trimmed = rest.trim_start();
        let separator = rest_trimmed
            .strip_prefix(':')
            .or_else(|| rest_trimmed.strip_prefix("\u{2014}"))
            .or_else(|| rest_trimmed.strip_prefix("\u{2013}"))
            .or_else(|| rest_trimmed.strip_prefix("- "));
        if let Some(content) = separator {
            return Some((kind, content.trim_start_matches(['*', '_'])));
        }
    }
    for (opener, drop_opener) in NEXT_PROSE {
        if lower.starts_with(opener) {
            let from = if *drop_opener {
                opener.len()
            } else {
                "next ".len()
            };
            return body.get(from..).map(|content| (ClosingKind::Next, content));
        }
    }
    None
}

/// The first two list items after a marker line that holds nothing itself,
/// joined; only items nested under it (indented deeper) count.
fn following_items(lines: &[&str], marker_indent: usize) -> String {
    let mut items: Vec<String> = Vec::new();
    for line in lines {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            break;
        }
        let indent = line.len() - trimmed.len();
        let (rest, bulleted, _) = strip_list_marker(trimmed);
        if !bulleted || indent <= marker_indent {
            break;
        }
        items.push(rest.trim().trim_end_matches(&[';', '.'][..]).to_string());
        if items.len() == 2 {
            break;
        }
    }
    items.join("; ")
}

/// Longest sentence a marker may quote.
const MARKER_SENTENCE_MAX_CHARS: usize = 160;

/// The first whole sentence of a marker's content, or `None`: a sentence too
/// long to quote whole abstains instead of being cut.
fn marker_sentence(content: &str) -> Option<String> {
    let flat = content.replace("**", "").replace("__", "");
    let sentence = crate::hooks::recap::first_sentence(&flat)?;
    (sentence.chars().count() <= MARKER_SENTENCE_MAX_CHARS
        && crate::hooks::recap::has_substance(&sentence)
        && !crate::hooks::recap::is_no_evidence_next(&sentence)
        && !sentence.ends_with('?')
        && !crate::import::contamination::has_csr_text(&sentence)
        && !crate::extraction::provenance::is_csr_emission(&sentence))
    .then_some(sentence)
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

    #[test]
    fn a_stop_or_an_output_read_closes_a_task() {
        let mut tracker = BackgroundTracker::default();
        launch(&mut tracker, "t1", "render reel", "bq1");
        launch(&mut tracker, "t2", "upload build", "bq2");
        launch(&mut tracker, "t3", "probe", "bq3");
        tracker.tool_use(&serde_json::json!({
            "type": "tool_use", "id": "x", "name": "TaskStop", "input": {"task_id": "bq1"}
        }));
        tracker.tool_use(&serde_json::json!({
            "type": "tool_use", "id": "y", "name": "Bash",
            "input": {"command": "tail -5 /private/tmp/claude/tasks/bq2.output"}
        }));
        let (open, total) = tracker.open();
        assert_eq!(total, 1);
        assert_eq!(open[0].id, "bq3");
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
        tracker.notification(
            "<task-notification><task-id>a4c996de2463be3d6</task-id></task-notification>",
        );
        assert_eq!(tracker.open().1, 0);
    }

    fn next(text: &str) -> Option<String> {
        closing_marker(text)
            .filter(|(kind, _)| *kind == ClosingKind::Next)
            .map(|(_, line)| line)
    }

    #[test]
    fn next_markers_in_the_shapes_real_closing_messages_use() {
        assert_eq!(
            next("Done.\n\n- **Next:** after you have used it, I will pull the app's tap log from the phone.").as_deref(),
            Some("after you have used it, I will pull the app's tap log from the phone.")
        );
        assert_eq!(
            next("Notes.\n\n**Next move:** listen to the first 3 seconds on a phone.").as_deref(),
            Some("listen to the first 3 seconds on a phone.")
        );
        assert_eq!(
            next("Next is your sign-off on v3 before any upscale or final export. If the titles bother you, re-fire.").as_deref(),
            Some("your sign-off on v3 before any upscale or final export.")
        );
        assert_eq!(
            next("The label server is rebuilt. Next I'll recover the set builder the same way.\n"),
            None,
            "a marker mid-line is prose, not a marker"
        );
        assert_eq!(
            next("The label server is rebuilt.\nNext I'll recover the set builder the same way.")
                .as_deref(),
            Some("I'll recover the set builder the same way.")
        );
    }

    #[test]
    fn still_open_is_the_open_kind_and_reads_nested_items() {
        let (kind, line) = closing_marker(
            "Done.\n- **Still waiting on you:**\n  - the `gcloud` login, so I can check traffic;\n  - your spend OK on the new opener.\n",
        )
        .unwrap();
        assert_eq!(kind, ClosingKind::Open);
        assert_eq!(
            line,
            "the `gcloud` login, so I can check traffic; your spend OK on the new opener"
        );
    }

    #[test]
    fn a_marker_inside_code_a_quote_a_numbered_list_or_prose_is_not_a_marker() {
        assert_eq!(next("```\nNext: drop the table\n```\n"), None);
        assert_eq!(next("> Next: drop the table"), None);
        assert_eq!(next("1. Fix it\n2. Next: drop the table"), None);
        assert_eq!(next("    Next: drop the table"), None);
        assert_eq!(
            next("The recap's Next: clause is reserved above the list."),
            None
        );
        assert_eq!(next("Next time, set the CapCut project to 30 fps."), None);
        assert_eq!(
            next("- **Remaining limit:** a pass means the code is unchanged."),
            None
        );
        assert_eq!(next("Next: nothing."), None);
        assert_eq!(next("Next: want me to also run the gate?"), None);
        assert_eq!(next("Next:"), None);
    }

    #[test]
    fn a_first_sentence_too_long_to_quote_whole_abstains() {
        let long = format!("Next: {}.", "word ".repeat(60));
        assert_eq!(next(&long), None);
    }

    #[test]
    fn csr_text_in_a_marker_line_is_not_quoted() {
        assert_eq!(next("Next: recap [2h ago]: fix login. [[CSR:RECAP]]"), None);
    }

    #[test]
    fn the_last_marker_wins() {
        assert_eq!(
            next("Next: first thing.\n\nLater.\n\nNext: second thing.").as_deref(),
            Some("second thing.")
        );
    }
}
