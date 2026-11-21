//! What a session started with: the facts the Context card shows before (and just after) the
//! first reply, from real data only.
//!
//! Claude Code writes a session's transcript when the first prompt is sent, so a session that has
//! not been prompted yet has no transcript at all. Two sources cover it:
//!
//! * the SessionStart hook leaves `<keep>/start/<sid>.json` (its own output's size, its recap
//!   clause), the one thing known at that moment;
//! * once the transcript exists, the rows before the first reply hold what else went in: CSR's
//!   hook text, the memory and CLAUDE.md files (`instructions`), the skill list and the system
//!   prompt. Their characters are counted by [`Start`] and sized as the breakdown sizes its
//!   parts (`CHARS_PER_TOKEN`), so every size here is an estimate and is marked so.
//!
//! A part nobody measured is `null` and the card says "unmeasured". Nothing is guessed.

use super::breakdown::CHARS_PER_TOKEN;
use super::paths::{valid_id, Paths};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::path::Path;

/// Where the hook leaves its note, under the companion's keep folder.
pub const FOLDER: &str = "start";
/// The card tells what the session started with while it has at most this many replies.
pub const FEW_REPLIES: usize = 3;
/// A note older than this is dropped by `prune`.
const KEEP_SECS: u64 = 3 * 86400;

/// What CSR's SessionStart text carries: one of these marks, under a first line of its own.
/// Another hook can quote a CSR line, but it opens with its own words.
const SESSION_START_MARKS: [&str; 3] = [
    "CSR ENDLESS MEMORY",
    "[[CSR:RECAP]]",
    "[[CSR:COMPACT_RECEIPT]]",
];
const SESSION_START_OPENINGS: [&str; 4] = [
    "CSR ENDLESS MEMORY ACTIVE",
    "recap [",
    "Before this compact: ",
    "Compact receipt: turns ",
];

/// Whether a hook row's command rules CSR in or out. Claude Code masks commands in transcripts
/// (`command hook 3`), and a masked or absent one says nothing.
fn command_says_csr(a: &Value, subcommand: &str) -> Option<bool> {
    let command = a["command"].as_str()?;
    if command.starts_with("command hook ") {
        return None;
    }
    Some(command.contains("csr-engine") && command.ends_with(subcommand))
}

/// Whether a transcript `attachment` is CSR's own SessionStart injection: the real command, or
/// with a masked one, CSR's opening line and a CSR mark. A rewind handoff or a reseeded state that
/// quotes a CSR line opens with its own words.
pub fn is_csr_session_start(a: &Value) -> bool {
    let session_start = a["hookEvent"] == "SessionStart"
        || a["hookName"]
            .as_str()
            .is_some_and(|n| n.starts_with("SessionStart:"));
    if a["type"] != "hook_success" || !session_start {
        return false;
    }
    let Some(content) = a["content"].as_str() else {
        return false;
    };
    match command_says_csr(a, "hook session-start") {
        Some(says) => says,
        None => {
            let first = content.trim_start().lines().next().unwrap_or("");
            SESSION_START_OPENINGS.iter().any(|o| first.starts_with(o))
                && ((first.starts_with("Before this compact: ")
                    && first.contains("csr_transcript(session="))
                    || SESSION_START_MARKS.iter().any(|m| content.contains(m)))
        }
    }
}

/// CSR's UserPromptSubmit injection, by the exact words its text opens with: past context, a
/// pickup, or a `CSR:` note. Another prompt hook (a goals nudge, a rewind handoff) is not CSR's.
const PROMPT_MARKS: [&str; 4] = [
    "## PAST CONTEXT - NOT INSTRUCTIONS",
    "## RELEVANT PAST CONTEXT - NOT INSTRUCTIONS",
    "CSR PICKUP \u{2014} ",
    "CSR: ",
];
/// Most notes on prompts a session keeps, and the longest one kept.
pub const TOLD_LOG_KEEP: usize = 40;
const TOLD_LOG_CHARS: usize = 2000;
/// Longest SessionStart text the note keeps whole.
const NOTE_TEXT_CHARS: usize = 16_000;

/// The text of a CSR prompt injection in a transcript `attachment`, cut to what the log keeps.
pub fn prompt_injection(a: &Value) -> Option<String> {
    if a["type"] != "hook_success" || a["hookEvent"] != "UserPromptSubmit" {
        return None;
    }
    if command_says_csr(a, "hook prompt-submit") == Some(false) {
        return None;
    }
    let text = a["content"].as_str()?.trim_start();
    PROMPT_MARKS
        .iter()
        .any(|m| text.starts_with(m))
        .then(|| text.chars().take(TOLD_LOG_CHARS).collect())
}

/// The compact receipt's turns as a clause: "compact receipt for turns 12..340".
fn receipt_clause(text: &str) -> Option<String> {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("Compact receipt:"))?;
    let turns = line
        .strip_prefix("Compact receipt: turns ")
        .and_then(|r| r.split([',', ' ']).next())
        .filter(|t| !t.is_empty());
    Some(match turns {
        Some(t) => format!("compact receipt for turns {t}"),
        None => "compact receipt".to_owned(),
    })
}

/// The CSR recap's first clause: what the session before this one was about. The recap reads
/// `recap [<age>]: <intent>: <completed>. ...`, so the clause is the intent, cut at the first
/// ": " or sentence end. With no recap line, a compact receipt's own clause. `None` when the
/// text holds neither.
pub fn recap_clause(text: &str) -> Option<String> {
    let Some(line) = text
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("recap ["))
    else {
        return receipt_clause(text);
    };
    let line = line.trim_end_matches("[[CSR:RECAP]]").trim();
    let (_, rest) = line.split_once("]: ")?;
    let end = [rest.find(": "), rest.find(". ")]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(rest.len());
    let clause = rest[..end].trim().trim_end_matches(['.', ':']).trim();
    (!clause.is_empty()).then(|| clause.to_owned())
}

/// How many earlier threads the episode index of the hook's text lists.
fn episode_lines(text: &str) -> usize {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    lines
        .iter()
        .position(|l| l.contains("EPISODE INDEX"))
        .map(|i| {
            lines[i + 1..]
                .iter()
                .filter(|l| l.starts_with("- ["))
                .count()
        })
        .unwrap_or(0)
}

/// The note the SessionStart hook keeps for the pane: the size of what it injected and its recap
/// clause.
pub fn note_of(source: &str, text: &str, at: f64) -> Value {
    json!({
        "at": at,
        "source": source,
        "chars": text.chars().count(),
        "recap": recap_clause(text),
        "episodes": episode_lines(text),
        "text": text.chars().take(NOTE_TEXT_CHARS).collect::<String>(),
    })
}

/// Make `path` a directory of mode 0700 if it is not one: one `mkdir`, never recursive, and an
/// existing directory is left as it is (no chmod, no walk). A link is refused.
fn private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
                return Err(std::io::Error::from_raw_os_error(libc::ELOOP));
            }
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Write `<keep>/start/<sid>.json` for the hook, which is on SessionStart's critical path: one
/// small file of its own, mode 0600, written to a temporary name and renamed into place, in a
/// directory created at 0700 if absent. It reads no directory, walks nothing and changes the
/// mode of nothing that exists; old notes are pruned by the pane (`prune`), never here.
pub fn write(keep: &Path, sid: &str, source: &str, text: &str, at: f64) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if !valid_id(sid) {
        bail!("start: refusing a conversation id that is not a plain name");
    }
    let folder = keep.join(FOLDER);
    private_dir(keep)?;
    private_dir(&folder)?;
    let temp = folder.join(format!(".{sid}.{}.tmp", std::process::id()));
    let body = note_of(source, text, at).to_string();
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temp)
        .and_then(|mut f| f.write_all(body.as_bytes()))
        .and_then(|()| std::fs::rename(&temp, folder.join(format!("{sid}.json"))));
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    Ok(written?)
}

/// Drop the notes older than `KEEP_SECS`. Run by the pane at its start on a thread of its own,
/// never by the hook and never in a frame.
pub fn prune(keep: &Path) {
    let Ok(entries) = std::fs::read_dir(keep.join(FOLDER)) else {
        return;
    };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age.as_secs() > KEEP_SECS);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// How many note files this process has read (a test counts them).
static READS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
pub fn reads() -> usize {
    READS.load(std::sync::atomic::Ordering::Relaxed)
}

/// The note of `sid` and its modification time, read only when the time is not `known`: the
/// time is one `stat`, the read happens once per change. `(None, None)`: there is no note.
pub fn read_changed(
    paths: &Paths,
    sid: &str,
    known: Option<std::time::SystemTime>,
) -> (Option<std::time::SystemTime>, Option<Value>) {
    let Some(path) = paths.kept_file(FOLDER, sid) else {
        return (None, None);
    };
    let Ok(modified) = std::fs::metadata(&path).and_then(|m| m.modified()) else {
        return (None, None);
    };
    if known == Some(modified) {
        return (Some(modified), None);
    }
    READS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    (Some(modified), Some(super::paths::read_json(&path)))
}

/// How often a running pane looks again at its session's start note: one `stat` on a thread of
/// its own, a read only when the file changed.
const START_NOTE_EVERY: f64 = 5.;
/// The session's SessionStart note, cached by (sid, modification time). A frame never touches
/// the file: a running pane refreshes it on a thread and takes the answer when it arrives; a
/// one-shot run (`--frame`, `--png`, `--json --cards`) has no draw loop to hold up and reads it
/// once. Moved here from pane.rs (over the 2,000-line cap).
#[derive(Default)]
pub(super) struct StartNote {
    sid: String,
    value: Value,
    modified: Option<std::time::SystemTime>,
    /// When the file was last looked at.
    checked: Option<f64>,
    wait: Option<std::sync::mpsc::Receiver<(Option<std::time::SystemTime>, Option<Value>)>>,
}
impl StartNote {
    pub(super) fn get(&mut self, paths: &Paths, sid: &str, persist: bool, now: f64) -> Value {
        if self.sid != sid {
            *self = Self {
                sid: sid.to_owned(),
                ..Self::default()
            };
        }
        if let Some(wait) = &self.wait {
            if let Ok((modified, value)) = wait.try_recv() {
                self.modified = modified;
                if let Some(value) = value {
                    self.value = value;
                }
                self.wait = None;
            }
        }
        let due = self
            .checked
            .is_none_or(|at| persist && now - at >= START_NOTE_EVERY);
        if due && self.wait.is_none() && !sid.is_empty() {
            self.checked = Some(now);
            if persist {
                let (send, receive) = std::sync::mpsc::channel();
                let (paths, sid, known) = (paths.clone(), sid.to_owned(), self.modified);
                std::thread::spawn(move || {
                    let _ = send.send(read_changed(&paths, &sid, known));
                });
                self.wait = Some(receive);
            } else {
                let (modified, value) = read_changed(paths, sid, self.modified);
                self.modified = modified;
                if let Some(value) = value {
                    self.value = value;
                }
            }
        }
        self.value.clone()
    }
}

/// Whether a note belongs to the session as it runs now. A reused sid (a resume) keeps its old
/// note until a new hook replaces it; a hook that failed or took the degraded path never does.
/// The note is from an earlier run when it is older than the process that runs the session
/// (`started`, from the live session record) or than the newest SessionStart text the transcript
/// kept (`told_at`). With neither known, nothing says it is old.
pub fn note_is_current(note: &Value, started: Option<f64>, told_at: Option<f64>) -> bool {
    let Some(at) = note["at"].as_f64() else {
        return false;
    };
    // The hook runs seconds after the process starts, and the transcript row is stamped as the
    // hook ends.
    const SLACK: f64 = 5.;
    started.is_none_or(|s| at + SLACK >= s) && told_at.is_none_or(|t| at + SLACK >= t)
}

/// The characters of what the first context held, counted from the rows before the first reply.
/// A part is `None` until a row of its kind was seen: "unmeasured", never zero by default.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Start {
    /// The hook text CSR injected at SessionStart.
    pub csr: Option<u64>,
    /// The memory and CLAUDE.md files (`instructions`).
    pub memory: Option<u64>,
    /// The skill list.
    pub skills: Option<u64>,
    /// The system prompt of the request.
    pub system: Option<u64>,
    /// The first reply's total context: the one real reading of the start.
    pub reading: Option<f64>,
    /// Counting ends at the first reply.
    frozen: bool,
}

impl Start {
    /// Count one main-thread transcript row. True when a figure changed.
    pub fn row(&mut self, row: &Value) -> bool {
        if self.frozen || row["type"] != "attachment" {
            return false;
        }
        let a = &row["attachment"];
        let len = |v: &Value| v.as_str().map_or(0, |s| s.chars().count() as u64);
        let (slot, n) = match a["type"].as_str().unwrap_or("") {
            "hook_success" if is_csr_session_start(a) => (&mut self.csr, len(&a["content"])),
            "instructions" => (
                &mut self.memory,
                a["files"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|f| len(&f["content"]))
                    .sum(),
            ),
            "skill_listing" => (&mut self.skills, len(&a["content"])),
            "prompt_snapshot" => (
                &mut self.system,
                a["systemPrompt"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(len)
                    .sum(),
            ),
            _ => return false,
        };
        *slot = Some(slot.unwrap_or(0) + n);
        true
    }
    /// The first main-thread reply reported `total` tokens of context: the reading of the start.
    pub fn reply(&mut self, total: f64) -> bool {
        if self.frozen {
            return false;
        }
        self.frozen = true;
        self.reading = Some(total).filter(|t| *t > 0.);
        true
    }
    pub fn json(&self) -> Value {
        json!({
            "csr": self.csr,
            "memory": self.memory,
            "skills": self.skills,
            "system": self.system,
            "reading": self.reading,
        })
    }
}

/// The Lost and found threads still open, newest first, each as the handoff popup takes it (its
/// words also under `line`): the same threads the monologue's rows open, whether or not that page
/// is open.
pub(crate) fn open_threads(lost: &Value) -> Vec<Value> {
    let mut out: Vec<Value> = lost
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| matches!(t["state"].as_str(), Some("lost" | "open" | "carried")))
        .filter(|t| !super::lost::is_summary_footer(t["text"].as_str().unwrap_or("")))
        .map(|t| {
            let mut item = t.clone();
            item["line"] = t["text"].clone();
            item
        })
        .collect();
    out.sort_by(|a, b| {
        b["last_seen"]
            .as_f64()
            .unwrap_or(0.)
            .total_cmp(&a["last_seen"].as_f64().unwrap_or(0.))
    });
    out
}

pub(crate) fn tokens(chars: u64) -> f64 {
    (chars as f64 / CHARS_PER_TOKEN).round()
}

/// The start facts for the figure, from the session's data `s`: its counted start (`start`), the
/// hook's note (`start_note`), the text the transcript kept of the hook (`told`) and the replies
/// so far. `Null` once the session is past its first few replies or has compacted: the card
/// tells the start of a new session only.
pub fn figure(s: &Value) -> Value {
    let replies = s["main"].as_array().map_or(0, Vec::len);
    let compacted = s["compacts"].as_array().is_some_and(|c| !c.is_empty());
    if s["id"].as_str().is_none_or(str::is_empty) || replies > FEW_REPLIES || compacted {
        return Value::Null;
    }
    let counted = &s["start"];
    // A note left by an earlier run of a reused sid is not this session's.
    let note = if note_is_current(
        &s["start_note"],
        s["record_started"].as_f64(),
        s["told"]["at"].as_f64(),
    ) {
        &s["start_note"]
    } else {
        &Value::Null
    };
    let told = s["told"]["text"].as_str().unwrap_or("");
    // CSR's text: counted from the transcript, else the hook's own note.
    let csr = counted["csr"].as_u64().or_else(|| note["chars"].as_u64());
    let recap = recap_clause(told).or_else(|| note["recap"].as_str().map(str::to_owned));
    let part = |key: &str, name: &str, chars: Option<u64>| json!({"key": key, "name": name, "tokens": chars.map(tokens)});
    let parts = vec![
        part("csr", "CSR injected", csr),
        part("memory", "Memory", counted["memory"].as_u64()),
        part("skills", "Skills", counted["skills"].as_u64()),
        part("system", "System prompt", counted["system"].as_u64()),
    ];
    let reading = counted["reading"].as_f64().filter(|n| *n > 0.);
    // With a reading, what the counted parts do not explain is the rest of the start: the
    // built-in tool schemas and whatever the transcript does not hold.
    let measured: f64 = parts.iter().filter_map(|p| p["tokens"].as_f64()).sum();
    let rest = reading
        .map(|n| (n - measured).max(0.))
        .filter(|n| *n > 0. && measured > 0.);
    json!({
        "replies": replies,
        "threads": open_threads(&s["lost"]),
        "reading": reading,
        "source": note["source"].as_str().or_else(|| s["told"]["source"].as_str()),
        "parts": parts,
        "rest": rest,
        "recap": recap,
        "episodes": note["episodes"],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOOK: &str = "CSR ENDLESS MEMORY ACTIVE\n\nrecap [1m ago]: why did our MCP tools vanish from Claude Code while it still said Connected: Checking the memory note. [[CSR:RECAP]]\nDreamt: x\nEPISODE INDEX — earlier threads\n- [3m ago] a\n- [1h ago] b\n";

    fn hook_row(content: &str) -> Value {
        json!({"type":"attachment","attachment":{"type":"hook_success","hookEvent":"SessionStart",
            "command":"/usr/local/bin/csr-engine hook session-start","content":content}})
    }

    #[test]
    fn test_the_recap_clause_is_the_intent_before_the_first_colon_or_sentence() {
        assert_eq!(
            recap_clause(HOOK).as_deref(),
            Some("why did our MCP tools vanish from Claude Code while it still said Connected")
        );
        assert_eq!(
            recap_clause(
                "recap [28m ago]: Fix the recap composer. Latest: x (abc1234). [[CSR:RECAP]]"
            )
            .as_deref(),
            Some("Fix the recap composer")
        );
        assert_eq!(
            recap_clause("recap [3h ago]: anthropics codemod, github links. [[CSR:RECAP]]")
                .as_deref(),
            Some("anthropics codemod, github links")
        );
        assert_eq!(recap_clause("no recap here"), None);
    }

    fn masked_row(content: &str, name: &str) -> Value {
        json!({"type":"hook_success","hookEvent":"SessionStart","hookName":name,
            "command":"command hook 3","content":content})
    }

    #[test]
    fn test_a_masked_command_is_still_csrs_when_the_text_carries_csr_marks() {
        // the old form: the real command
        assert!(is_csr_session_start(&hook_row("plain text")["attachment"]));
        // the masked form, each mark
        for content in [
            "CSR ENDLESS MEMORY ACTIVE — x",
            "recap [1m ago]: a [[CSR:RECAP]]",
            "Compact receipt: turns 1..2 [[CSR:COMPACT_RECEIPT]]",
        ] {
            assert!(
                is_csr_session_start(&masked_row(content, "SessionStart:compact")),
                "{content}"
            );
        }
        // another hook's SessionStart text is not CSR's, masked or not
        for content in [
            "REWIND HANDOFF - do this before the user truncates",
            "## RESEEDED STATE (from handoff note written 4 min ago)",
        ] {
            assert!(
                !is_csr_session_start(&masked_row(content, "SessionStart:compact")),
                "{content}"
            );
        }
        // another hook's text that opens with the pointer's words, and has no way back to the turns
        assert!(!is_csr_session_start(&masked_row(
            "Before this compact: the deployment was blocked.",
            "SessionStart:compact"
        )));
        // the pointer with no turn range, for a transcript too large to parse
        let fallback = crate::transcript::compact_pointer_line("s", None);
        assert!(is_csr_session_start(&masked_row(
            &fallback,
            "SessionStart:compact"
        )));
        // another hook that quotes a CSR line opens with its own words
        let quoting = "REWIND HANDOFF\nPrevious context:\nrecap [1m ago]: deploy [[CSR:RECAP]]";
        assert!(!is_csr_session_start(&masked_row(
            quoting,
            "SessionStart:compact"
        )));
        // a command that can be read and is another hook's is not CSR's, whatever the text
        let mut named = masked_row("CSR ENDLESS MEMORY ACTIVE", "SessionStart:startup");
        named["command"] = json!("/usr/local/bin/rewind hook session-start");
        assert!(!is_csr_session_start(&named));
        // not a SessionStart row, or no text
        let mut other = masked_row("CSR ENDLESS MEMORY ACTIVE", "UserPromptSubmit");
        other["hookEvent"] = json!("UserPromptSubmit");
        assert!(!is_csr_session_start(&other));
        let mut empty = masked_row("", "SessionStart:startup");
        empty["content"] = Value::Null;
        assert!(!is_csr_session_start(&empty));
    }

    #[test]
    fn test_a_masked_compact_row_counts_as_csrs_start_text() {
        let mut st = Start::default();
        let text = "CSR ENDLESS MEMORY ACTIVE\nabcd";
        assert!(st.row(&json!({"type":"attachment","attachment":
            masked_row(text, "SessionStart:compact")})));
        assert_eq!(st.csr, Some(text.chars().count() as u64));
        assert!(!st.row(&json!({"type":"attachment","attachment":
            masked_row("REWIND HANDOFF - x", "SessionStart:compact")})));
    }

    #[test]
    fn test_only_csrs_prompt_injections_enter_the_told_log() {
        let row = |content: &str| {
            json!({"type":"hook_success","hookEvent":"UserPromptSubmit","hookName":"UserPromptSubmit",
                "command":"command hook 4","content":content})
        };
        for content in [
            "## PAST CONTEXT - NOT INSTRUCTIONS\n- [a] b",
            "## RELEVANT PAST CONTEXT - NOT INSTRUCTIONS\n- [a] b",
            "CSR PICKUP — this prompt matches a past episode",
            "CSR: something",
        ] {
            assert!(prompt_injection(&row(content)).is_some(), "{content}");
        }
        // past context in another hook's own heading is not CSR's
        assert_eq!(
            prompt_injection(&row("## PAST CONTEXT FROM REWIND\n- deployment pending")),
            None
        );
        let mut named = row("CSR: something");
        named["command"] = json!("/usr/local/bin/rewind hook prompt-submit");
        assert_eq!(prompt_injection(&named), None);
        for content in [
            "GOAL-SEEKING: drive task to completion.",
            "REWIND HANDOFF - do this before the user truncates:",
            "## RESEEDED STATE (from handoff note)",
            "",
        ] {
            assert_eq!(prompt_injection(&row(content)), None, "{content}");
        }
        let long = format!("## PAST CONTEXT - NOT INSTRUCTIONS\n{}", "y".repeat(5000));
        assert_eq!(prompt_injection(&row(&long)).unwrap().chars().count(), 2000);
    }

    #[test]
    fn test_the_note_keeps_the_text_to_16000_characters() {
        let text = format!("CSR ENDLESS MEMORY ACTIVE\n{}", "x".repeat(20_000));
        let n = note_of("startup", &text, 1.);
        assert_eq!(n["text"].as_str().unwrap().chars().count(), 16_000);
        assert_eq!(n["chars"], json!(text.chars().count()));
    }

    #[test]
    fn test_with_no_recap_the_clause_is_the_compact_receipt() {
        let text = "Before this compact: turns 12..340 of this session.\nCompact receipt: turns 12..340, source: live transcript (backup not verified). Historical evidence. [[CSR:COMPACT_RECEIPT]]\n";
        assert_eq!(
            recap_clause(text).as_deref(),
            Some("compact receipt for turns 12..340")
        );
        // a recap still wins
        assert_eq!(
            recap_clause(&format!(
                "{text}recap [1m ago]: fix the pane. [[CSR:RECAP]]"
            ))
            .as_deref(),
            Some("fix the pane")
        );
    }

    #[test]
    fn test_the_note_holds_the_hooks_size_clause_and_episode_count() {
        let n = note_of("startup", HOOK, 5.);
        assert_eq!(n["chars"], json!(HOOK.chars().count()));
        assert_eq!(n["episodes"], json!(2));
        assert_eq!(n["source"], "startup");
        assert!(n["recap"].as_str().unwrap().starts_with("why did our MCP"));
    }

    #[test]
    fn test_the_note_is_written_privately_and_read_back_and_a_bad_id_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let paths = Paths::new(d.path().to_path_buf());
        // The data directory exists (the database is in it); the hook makes only what is under it.
        std::fs::create_dir_all(paths.keep.parent().unwrap()).unwrap();
        write(&paths.keep, "sess-1", "startup", HOOK, 7.).unwrap();
        let (_, got) = read_changed(&paths, "sess-1", None);
        assert_eq!(got.unwrap()["chars"], json!(HOOK.chars().count()));
        assert_eq!(read_changed(&paths, "nope", None), (None, None));
        assert!(write(&paths.keep, "../x", "startup", HOOK, 7.).is_err());
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&paths.keep.join(FOLDER).join("sess-1.json")), 0o600);
        assert_eq!(mode(&paths.keep.join(FOLDER)), 0o700);
        // No temporary name is left behind.
        let names: Vec<_> = std::fs::read_dir(paths.keep.join(FOLDER))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(names.len(), 1);
    }

    /// The hook's write does the same work with 5000 notes beside it as with none: it reads no
    /// directory (the folder is made unreadable, so a listing would fail), changes no mode of
    /// anything that exists, and makes no `.private` marker.
    #[test]
    fn test_the_hooks_write_touches_only_its_own_note_whatever_is_beside_it() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let keep = d.path().join("csr-companion");
        let folder = keep.join(FOLDER);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::set_permissions(&keep, std::fs::Permissions::from_mode(0o755)).unwrap();
        for i in 0..5000 {
            let p = folder.join(format!("old-{i}.json"));
            std::fs::write(&p, "{}").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        // Write and search, no read: a listing of this folder would be refused.
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o300)).unwrap();
        write(&keep, "fresh", "startup", HOOK, 9.).unwrap();
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 5001);
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode(&folder.join("old-4999.json")),
            0o644,
            "a mode was changed"
        );
        assert_eq!(mode(&keep), 0o755, "the parent was tightened");
        assert!(!keep.join(".private").exists());
        assert!(folder.join("fresh.json").exists());
    }

    #[test]
    fn test_prune_drops_only_old_notes() {
        let d = tempfile::tempdir().unwrap();
        let keep = d.path().join("csr-companion");
        write(&keep, "new", "startup", HOOK, 1.).unwrap();
        write(&keep, "old", "startup", HOOK, 1.).unwrap();
        let old = keep.join(FOLDER).join("old.json");
        let when = std::time::SystemTime::now() - std::time::Duration::from_secs(KEEP_SECS + 60);
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(when)
            .unwrap();
        prune(&keep);
        assert!(!old.exists());
        assert!(keep.join(FOLDER).join("new.json").exists());
    }

    #[test]
    fn test_a_note_older_than_the_process_or_the_newest_start_text_is_not_this_sessions() {
        let note = json!({"at": 1000.});
        // The live record says the process started well after the note was written: a resume.
        assert!(!note_is_current(&note, Some(5000.), None));
        // The hook ran a moment after the process started: current.
        assert!(note_is_current(&note, Some(998.), None));
        // The transcript kept a newer SessionStart text than the note.
        assert!(!note_is_current(&note, None, Some(1500.)));
        assert!(note_is_current(&note, None, Some(1002.)));
        // Nothing says it is old; no `at` is no note.
        assert!(note_is_current(&note, None, None));
        assert!(!note_is_current(&Value::Null, None, None));
        // The figure ignores a stale note: no CSR size and no recap from it.
        let s = json!({"id":"s1","main":[],"compacts":[],"record_started":5000.,
            "start_note": note_of("startup", HOOK, 1000.)});
        let f = figure(&s);
        assert_eq!(f["recap"], Value::Null);
        assert!(f["parts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["tokens"].is_null()));
        let s = json!({"id":"s1","main":[],"compacts":[],"record_started":999.,
            "start_note": note_of("startup", HOOK, 1000.)});
        assert!(figure(&s)["recap"].is_string());
    }

    #[test]
    fn test_the_start_counts_only_what_it_saw_and_stops_at_the_first_reply() {
        let mut st = Start::default();
        assert_eq!(
            st.json()["csr"],
            Value::Null,
            "unseen is unmeasured, not zero"
        );
        assert!(st.row(&hook_row("abcd")));
        assert!(st.row(
            &json!({"type":"attachment","attachment":{"type":"instructions",
            "files":[{"content":"aaaa"},{"content":"bb"}]}})
        ));
        assert!(st.row(
            &json!({"type":"attachment","attachment":{"type":"skill_listing","content":"xyz"}})
        ));
        assert!(!st.row(&json!({"type":"user"})));
        assert!(!st.row(
            &json!({"type":"attachment","attachment":{"type":"hook_success",
            "hookEvent":"SessionStart","command":"other hook","content":"zzzz"}})
        ));
        assert_eq!(
            (st.csr, st.memory, st.skills, st.system),
            (Some(4), Some(6), Some(3), None)
        );
        assert!(st.reply(30_000.));
        assert!(!st.row(&hook_row("late")), "frozen at the first reply");
        assert_eq!(st.csr, Some(4));
        assert_eq!(st.reading, Some(30_000.));
    }

    #[test]
    fn test_the_figure_has_no_reading_and_unmeasured_parts_before_any_transcript() {
        let s =
            json!({"id":"s1","main":[],"compacts":[],"start_note":note_of("startup", HOOK, 1.)});
        let f = figure(&s);
        assert_eq!(f["reading"], Value::Null);
        assert_eq!(f["replies"], json!(0));
        let parts = f["parts"].as_array().unwrap();
        let by = |k: &str| parts.iter().find(|p| p["key"] == k).unwrap()["tokens"].clone();
        assert_eq!(
            by("csr"),
            json!((HOOK.chars().count() as f64 / CHARS_PER_TOKEN).round())
        );
        assert_eq!(by("memory"), Value::Null);
        assert_eq!(by("skills"), Value::Null);
        assert_eq!(by("system"), Value::Null);
        assert_eq!(f["rest"], Value::Null);
        assert!(f["recap"].as_str().unwrap().starts_with("why did our MCP"));
        assert_eq!(f["episodes"], json!(2));
    }

    #[test]
    fn test_the_open_threads_are_the_monologues_lost_open_and_carried_ones_newest_first() {
        let t = |text: &str, state: &str, seen: f64| json!({"key": text, "text": text, "state": state, "last_seen": seen});
        let s = json!({"id":"s1","main":[],"compacts":[],"lost":[
            t("old lost", "lost", 1.), t("answered one", "answered", 9.),
            t("new carried", "carried", 5.), t("faded one", "faded", 8.),
            t("mid open", "open", 3.)]});
        let f = figure(&s);
        let texts: Vec<_> = f["threads"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["line"].as_str().unwrap())
            .collect();
        assert_eq!(texts, ["new carried", "mid open", "old lost"]);
    }

    #[test]
    fn test_the_figure_with_a_reading_names_the_rest_and_ends_with_the_first_replies() {
        let mut st = Start::default();
        st.row(&hook_row(&"x".repeat(3600)));
        st.row(&json!({"type":"attachment","attachment":{"type":"instructions","files":[{"content":"y".repeat(7200)}]}}));
        st.reply(20_000.);
        let mut s = json!({"id":"s1","main":[[1.,20000]],"compacts":[],"start":st.json()});
        let f = figure(&s);
        assert_eq!(f["reading"], json!(20_000.));
        assert_eq!(f["rest"], json!(16_400.));
        // Past the first few replies, or after a compaction, the start is no longer told.
        s["main"] = json!([[1., 1], [2., 1], [3., 1], [4., 1]]);
        assert_eq!(figure(&s), Value::Null);
        s["main"] = json!([[1., 1]]);
        s["compacts"] = json!([9.]);
        assert_eq!(figure(&s), Value::Null);
        assert_eq!(figure(&json!({"id":"","main":[]})), Value::Null);
    }
}
