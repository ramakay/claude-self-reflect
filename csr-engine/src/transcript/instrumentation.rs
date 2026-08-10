//! Session instrumentation — tool-verified error evidence and mid-flight
//! human steers, computed once from an already-parsed transcript.
//!
//! Journal v2 (`.plans/journal-v2-mailbox-plan.md` §3.3) needs a number that
//! is provably NOT `error_signatures.len()` — the existing fallback counts
//! distinct substring-matched signature *strings* over merged tool_result
//! text (`"error["`, `"Error:"`, `"panic"`, …), which over-counts (a passing
//! test named `test_fail_case`, a diff containing the literal text
//! `"Error:"`) and under-counts (dedup collapses repeats). This module
//! counts what actually happened: `tool_result` blocks whose `is_error`
//! field is `true`, full stop.
//!
//! **One function, two call sites** (plan §3.3): [`from_parsed`] is called
//! both by the Stop hook's forward path (`hooks::stop::extract_and_store_episode`)
//! and, in a later phase, by the `dream --report` backfill — so the two
//! numbers can never disagree with each other the way two independent
//! implementations could.
//!
//! Steer detection reuses the exact same self-contamination guard the
//! `request`/`completed` episode fields already use
//! (`crate::extraction::provenance::extractable` /
//! `crate::extraction::provenance::is_csr_emission`) — without it, CSR's own
//! SessionStart recap injection would be narrated back as a human
//! mid-session correction (repo `CLAUDE.md`'s documented 4.4%
//! self-contamination channel; plan §8 R7).

use serde::{Deserialize, Serialize};

use super::{query, truncate_chars, ParsedTranscript, Role};

/// Shared skip bound (plan §3.3a cost table): above this size an extra
/// streaming pass over a transcript is skipped rather than paying an
/// unbounded cost. One constant, two call sites (`hooks::stop`'s forward
/// path and `dream::report`'s backfill path) so the bound can never drift
/// between the two the way two independently-declared constants could.
pub(crate) const MAX_TRANSCRIPT_SCAN_BYTES: u64 = 64 * 1024 * 1024;

/// Cap on `ErrorEvent::preview` — char-boundary safe (plan §3.3a).
const ERROR_PREVIEW_CHARS: usize = 160;
/// Cap on `SteerEvent::text` — char-boundary safe (plan §3.3b).
const STEER_TEXT_CHARS: usize = 120;
/// `top_errors` / `steers` are both bounded to the first/top 3 (plan §3.3).
const MAX_STORED_ERRORS: usize = 3;
const MAX_STORED_STEERS: usize = 3;

/// True if `text` is harness-generated plumbing shaped like a human steer but
/// never typed by one: a background task-completion notification injected as
/// a user turn, a `[SYSTEM NOTIFICATION - NOT USER INPUT]` block, a leaked
/// `<local-command-caveat>` wrapper, or `<command-name>` command plumbing.
/// None of the first three register in `extraction::provenance`'s
/// `strip_plumbing` tag list or `is_csr_emission`'s header/token registries —
/// that module owns CSR's OWN emissions and Claude Code's known command
/// wrappers, not the harness's task/session-management XML, so a live report
/// showed a raw `<task-notification>` block and a `[SYSTEM NOTIFICATION`
/// block counted as mid-flight human steers. `<command-name>` is already
/// stripped to empty by `extractable` (kept here too so this predicate is
/// self-sufficient at its second call site — the report renderer re-applying
/// it to steers a session may have already persisted under the pre-fix
/// filter, per plan Part A).
///
/// **One predicate, two call sites**: [`from_parsed`]'s steer loop (forward
/// path) and `dream::report`'s STEER stage-card builder (render-time
/// re-filter of already-stored steers).
pub(crate) fn is_noisy_steer_text(text: &str) -> bool {
    text.trim_start().starts_with("[SYSTEM NOTIFICATION")
        || text.contains("<task-notification>")
        || text.contains("<local-command-caveat>")
        || text.contains("<command-name>")
}

/// One `tool_result` block with `is_error: true`, resolved to its owning
/// tool's real name via the `tool_use_id` index (`"unknown"` when unpaired —
/// matching `transcript::query::render_errors`'s existing convention).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorEvent {
    pub turn: u32,
    pub tool: String,
    pub preview: String,
}

/// One user turn after the session's opening ask — a mid-flight
/// intervention/correction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteerEvent {
    pub turn: u32,
    pub text: String,
}

/// Per-session instrumentation computed by streaming an already-parsed
/// transcript once. See module doc + plan §3.3.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SessionInstrumentation {
    /// Total `tool_result` blocks with `is_error: true`. Tool-verified, not
    /// substring-matched — see module doc.
    pub error_count: u32,
    /// The top (by `byte_size` desc, then turn asc) up to 3 errors.
    pub top_errors: Vec<ErrorEvent>,
    /// Total user turns after the opening ask that survived provenance
    /// filtering.
    pub steer_count: u32,
    /// The first up to 3 steers, in transcript order.
    pub steers: Vec<SteerEvent>,
    /// Total recognized transcript entries (`ParsedTranscript::entries.len()`).
    /// Stored so a later baseline-relative feature (plan §9 Q6) does not need
    /// a second migration.
    pub turn_count: u32,
}

/// Compute [`SessionInstrumentation`] from an already-parsed transcript.
/// Pure, no I/O.
///
/// Returns measured zeros for an empty transcript — **never** treats
/// "nothing found" as "not measured". `Option`-shaped absence (`None` =
/// "never scanned this transcript at all") is a decision made by the
/// *caller* (`hooks::stop::extract_and_store_episode`), one layer up; this
/// function's contract is unconditional and always returns a concrete,
/// fully-measured value (plan §3.3, test `empty_transcript_yields_zeroes_not_none`).
pub fn from_parsed(parsed: &ParsedTranscript) -> SessionInstrumentation {
    let turn_count = parsed.entries.len() as u32;
    let tool_names = query::index_tool_use_names(&parsed.entries);

    // ---- errors: tool_result blocks with is_error:true, nothing else. ----
    // (byte_size, event) pairs so sorting can use the full byte_size even
    // though ErrorEvent itself does not carry it (plan struct shape).
    let mut error_candidates: Vec<(usize, ErrorEvent)> = Vec::new();
    for entry in &parsed.entries {
        for tr in &entry.tool_results {
            if !tr.is_error {
                continue;
            }
            let tool = tr
                .tool_use_id
                .as_deref()
                .and_then(|id| tool_names.get(id))
                .cloned()
                .unwrap_or_else(|| "unknown".to_string());
            error_candidates.push((
                tr.byte_size,
                ErrorEvent {
                    turn: entry.turn as u32,
                    tool,
                    preview: truncate_chars(&tr.preview, ERROR_PREVIEW_CHARS),
                },
            ));
        }
    }
    let error_count = error_candidates.len() as u32;
    // byte_size desc, then turn asc.
    error_candidates.sort_by(|(a_size, a_ev), (b_size, b_ev)| {
        b_size.cmp(a_size).then_with(|| a_ev.turn.cmp(&b_ev.turn))
    });
    let top_errors: Vec<ErrorEvent> = error_candidates
        .into_iter()
        .take(MAX_STORED_ERRORS)
        .map(|(_, ev)| ev)
        .collect();

    // ---- steers: user turns after the first accepted prompt. ----
    let mut steer_count = 0u32;
    let mut steers: Vec<SteerEvent> = Vec::new();
    let mut seen_ask = false;
    for entry in &parsed.entries {
        if entry.role != Role::User {
            continue;
        }
        // tool_result-only / genuinely blank user turns are plumbing, not
        // authored text.
        if entry.is_empty_of_content() {
            continue;
        }
        // Guard on the raw text directly, in addition to the check
        // `extractable` performs internally on its cleaned output below —
        // belt-and-suspenders per plan §3.3b/§8 R7 (non-negotiable): a
        // self-contamination echo must not become a steer regardless of
        // which stage of the pipeline would have caught it.
        if crate::extraction::provenance::is_csr_emission(&entry.text) {
            continue;
        }
        // Harness noise (task-completion notifications, system-injected
        // non-user-input blocks) shaped like a real user turn — see
        // `is_noisy_steer_text` doc.
        if is_noisy_steer_text(&entry.text) {
            continue;
        }
        let Some(cleaned) = crate::extraction::provenance::extractable(&entry.text) else {
            continue;
        };
        if !seen_ask {
            // The first surviving user turn is the ASK, not a steer.
            seen_ask = true;
            continue;
        }
        steer_count += 1;
        if steers.len() < MAX_STORED_STEERS {
            steers.push(SteerEvent {
                turn: entry.turn as u32,
                text: truncate_chars(&cleaned, STEER_TEXT_CHARS),
            });
        }
    }

    SessionInstrumentation {
        error_count,
        top_errors,
        steer_count,
        steers,
        turn_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::provenance::RECAP_SENTINEL;
    use crate::transcript::{Entry, ToolResult, ToolUse};

    fn parsed(entries: Vec<Entry>) -> ParsedTranscript {
        ParsedTranscript {
            entries,
            ..Default::default()
        }
    }

    fn user_text(turn: usize, text: &str) -> Entry {
        Entry {
            turn,
            role: Role::User,
            timestamp: None,
            uuid: None,
            is_sidechain: false,
            text: text.to_string(),
            tool_uses: vec![],
            tool_results: vec![],
        }
    }

    fn assistant_tool_use(turn: usize, id: &str, name: &str) -> Entry {
        Entry {
            turn,
            role: Role::Assistant,
            timestamp: None,
            uuid: None,
            is_sidechain: false,
            text: String::new(),
            tool_uses: vec![ToolUse {
                id: Some(id.to_string()),
                name: name.to_string(),
                file_path: None,
                command: None,
                pattern: None,
                prompt: None,
            }],
            tool_results: vec![],
        }
    }

    fn user_tool_result(
        turn: usize,
        tool_use_id: Option<&str>,
        is_error: bool,
        byte_size: usize,
        preview: &str,
    ) -> Entry {
        Entry {
            turn,
            role: Role::User,
            timestamp: None,
            uuid: None,
            is_sidechain: false,
            text: String::new(),
            tool_uses: vec![],
            tool_results: vec![ToolResult {
                tool_use_id: tool_use_id.map(str::to_string),
                is_error,
                byte_size,
                preview: preview.to_string(),
            }],
        }
    }

    // --- errors ---

    #[test]
    fn error_count_counts_only_is_error_tool_results() {
        let entries = vec![
            assistant_tool_use(1, "tu1", "Bash"),
            user_tool_result(2, Some("tu1"), true, 10, "boom"),
            assistant_tool_use(3, "tu2", "Edit"),
            user_tool_result(4, Some("tu2"), true, 20, "fail2"),
            assistant_tool_use(5, "tu3", "Read"),
            user_tool_result(6, Some("tu3"), true, 30, "fail3"),
            assistant_tool_use(7, "tu4", "Grep"),
            user_tool_result(8, Some("tu4"), false, 5, "ok clean"),
            assistant_tool_use(9, "tu5", "Write"),
            user_tool_result(10, Some("tu5"), false, 5, "ok clean 2"),
            assistant_tool_use(11, "tu6", "Task"),
            // is_error:false but preview merely CONTAINS "Error:" — proves
            // we are not re-implementing the error_signatures substring scan.
            user_tool_result(12, Some("tu6"), false, 40, "Error: but tool succeeded"),
        ];
        let inst = from_parsed(&parsed(entries));
        assert_eq!(inst.error_count, 3);
    }

    #[test]
    fn top_errors_are_bounded_named_and_ordered() {
        let multibyte_text: String = "错".repeat(200); // multi-byte, > 160 chars
        let entries = vec![
            assistant_tool_use(1, "tu1", "Bash"),
            user_tool_result(2, Some("tu1"), true, 300, "medium"),
            assistant_tool_use(3, "tu2", "Edit"),
            user_tool_result(4, Some("tu2"), true, 700, &multibyte_text),
            assistant_tool_use(5, "tu3", "Read"),
            user_tool_result(6, Some("tu3"), true, 700, "tied-later-turn"),
            assistant_tool_use(7, "tu4", "Grep"),
            user_tool_result(8, Some("tu4"), true, 100, "smallest"),
        ];
        let inst = from_parsed(&parsed(entries));
        assert_eq!(inst.error_count, 4);
        assert!(inst.top_errors.len() <= 3);
        // byte_size desc, ties by turn asc: turn4(700,Edit) < turn6(700,Read) < turn2(300,Bash)
        assert_eq!(inst.top_errors[0].turn, 4);
        assert_eq!(inst.top_errors[0].tool, "Edit");
        assert_eq!(inst.top_errors[1].turn, 6);
        assert_eq!(inst.top_errors[1].tool, "Read");
        assert_eq!(inst.top_errors[2].turn, 2);
        assert_eq!(inst.top_errors[2].tool, "Bash");
        // char-boundary safe: multi-byte input never panics, cap respected.
        assert!(inst.top_errors[0].preview.chars().count() <= ERROR_PREVIEW_CHARS + 1);
    }

    #[test]
    fn unpaired_error_result_still_counts_with_unknown_tool() {
        let entries = vec![
            // tool_use_id set but no matching tool_use anywhere.
            user_tool_result(1, Some("ghost-id"), true, 50, "orphaned error"),
            // no tool_use_id at all.
            user_tool_result(2, None, true, 20, "no id"),
        ];
        let inst = from_parsed(&parsed(entries));
        assert_eq!(inst.error_count, 2);
        assert!(inst.top_errors.iter().all(|e| e.tool == "unknown"));
    }

    // --- steers ---

    #[test]
    fn first_user_prompt_is_the_ask_not_a_steer() {
        let entries = vec![
            user_text(1, "make the podcast episode"),
            user_text(2, "set the hindi voice id"),
            user_text(3, "re-render at 12 min"),
            user_text(4, "upload to the app"),
        ];
        let inst = from_parsed(&parsed(entries));
        assert_eq!(inst.steer_count, 3);
        assert_eq!(inst.steers.len(), 3);
        assert_eq!(inst.steers[0].turn, 2);
    }

    #[test]
    fn steers_exclude_tool_result_only_user_turns() {
        let entries = vec![
            user_text(1, "make the podcast episode"),
            user_tool_result(2, Some("tu1"), false, 5, "ok"),
            user_text(3, "no — hindi, not english"),
        ];
        let inst = from_parsed(&parsed(entries));
        assert_eq!(inst.steer_count, 1);
        assert_eq!(inst.steers.len(), 1);
        assert_eq!(inst.steers[0].turn, 3);
    }

    #[test]
    fn steers_exclude_csr_emissions_and_command_plumbing() {
        let entries = vec![
            user_text(1, "make the podcast episode"),
            // sentinel-bearing recap echo
            user_text(
                2,
                &format!("recap [2h ago]: fixed it: shipped it. {RECAP_SENTINEL}"),
            ),
            // command wrapper plumbing — strips to nothing
            user_text(3, "<command-name>/memory-feedback</command-name>"),
            // pasted CSR search-result output
            user_text(
                4,
                "RELEVANT PAST CONTEXT\n- Past session [abc123] (outcome=success)\ncsr_reflect_on_past(query='foo')",
            ),
            // the one genuine positive case
            user_text(5, "no — hindi, not english"),
        ];
        let inst = from_parsed(&parsed(entries));
        assert_eq!(inst.steer_count, 1, "only the genuine turn should count");
        assert_eq!(inst.steers.len(), 1);
        assert_eq!(inst.steers[0].turn, 5);
        assert!(inst.steers[0].text.contains("hindi"));
    }

    #[test]
    fn steers_exclude_task_notifications_and_system_blocks() {
        let entries = vec![
            user_text(1, "make the podcast episode"),
            // [queued] background-task-completion notification injected as a
            // user turn — real shape observed in a live corpus.
            user_text(
                2,
                "[queued] <task-notification>\n<task-id>ac58728b340b16885</task-id>\n\
                 <tool-use-id>toolu_01QwFvBiPYrLbm7iEC3ta8Bq</tool-use-id>\n\
                 <status>completed</status>\n<summary>Agent finished</summary>\n\
                 </task-notification>",
            ),
            // harness system-injected "not user input" block.
            user_text(
                3,
                "[SYSTEM NOTIFICATION - NOT USER INPUT]\nThis is an automated \
                 background-task event, NOT a message from the user.",
            ),
            // leaked/unclosed local-command-caveat wrapper.
            user_text(
                4,
                "<local-command-caveat>Caveat: The messages below were generated \
                 by the user while running local commands. DO NOT respond",
            ),
            // skill-invocation command-name plumbing.
            user_text(5, "<command-name>/memory-feedback</command-name>"),
            // the one genuine positive control.
            user_text(6, "no — use hindi"),
        ];
        let inst = from_parsed(&parsed(entries));
        assert_eq!(
            inst.steer_count, 1,
            "only the genuine human steer should count"
        );
        assert_eq!(inst.steers.len(), 1);
        assert_eq!(inst.steers[0].turn, 6);
        assert!(inst.steers[0].text.contains("hindi"));
    }

    #[test]
    fn empty_transcript_yields_zeroes_not_none() {
        let inst = from_parsed(&ParsedTranscript::default());
        assert_eq!(inst.error_count, 0);
        assert!(inst.top_errors.is_empty());
        assert_eq!(inst.steer_count, 0);
        assert!(inst.steers.is_empty());
        assert_eq!(inst.turn_count, 0);
    }
}
