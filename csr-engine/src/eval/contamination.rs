//! Emitter contamination canary: every output CSR's own formatters can write
//! must be detected by the registry's grammars, and every way of merely
//! mentioning that output (a fence, a quote, a grep line, an assertion, an
//! analysis) must not be.
//!
//! The engine outputs below are produced by calling the production formatter
//! functions (or, for the two memory-feedback shapes that have no formatter in
//! this tree, their registered header constants). Nothing here copies an output
//! string, so a formatter that changes its wording turns the canary red until
//! the registry's grammar follows. The companion's sends come from
//! `plugins/csr-companion/tests/export-emissions.py`, which calls the pane's
//! own formatters; the plugin's TypeScript emitters appear as synthetic cases.
//!
//! A detected hit is a label, never an edit: every row in `cases.jsonl` expects
//! the input back byte for byte, with the reason the detector gives it.
//!
//! `cases.jsonl` is the checked-in fixture: the hand-written cases first, then
//! the materialised matrix (`matrix-*` rows), regenerated from this module by
//! `regenerate_fixture_matrix`. It holds no real transcript text.
//!
//! Deterministic and local: no database, model, network or embedding call.

use std::time::Instant;

use anyhow::{bail, Result};
use serde::Deserialize;

use super::EvalResult;
use crate::import::contamination::{detect, labelable, ContaminationReason, REGISTRY};
use crate::provenance::Speaker;

const CASES_JSONL: &str = include_str!("../../tests/fixtures/contamination/cases.jsonl");

/// What the companion's pane types into the prompt, written by the pane's own
/// formatters (`plugins/csr-companion/tests/export-emissions.py`). The plugin
/// keeps an identical copy; this one keeps the crate self-contained.
const PANE_JSON: &str = include_str!("../../tests/fixtures/contamination/pane-v1.json");

/// Prefix of the generated rows in `cases.jsonl`.
pub const MATRIX_PREFIX: &str = "matrix-";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, serde::Serialize)]
pub struct Case {
    pub id: String,
    pub source: String,
    pub channel: String,
    pub input: String,
    pub expected: String,
    pub reason: Option<ContaminationReason>,
}

pub fn cases() -> Vec<Case> {
    CASES_JSONL
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("cases.jsonl row: {e}")))
        .collect()
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CaseReport {
    pub total: usize,
    /// Rows whose `reason` is set: the detector must label them.
    pub labeled_rows: usize,
    /// Rows whose `reason` is null: no hit.
    pub unlabeled_rows: usize,
    pub passed: usize,
    pub failures: Vec<String>,
}

/// Whether a row's (source, channel) is one a chunk can carry CSR text in.
/// Only user and assistant text of a Claude transcript is; a tool result, a
/// Codex rollout, a plan, a CSR tool block or an unknown channel is a mention.
fn row_is_labelable(case: &Case) -> bool {
    let author = match case.channel.as_str() {
        "user_text" => Speaker::User,
        "assistant_text" => Speaker::Assistant,
        "non_csr_tool_result" => Speaker::ToolResult,
        _ => return false,
    };
    labelable(Some(&case.source), Some(author))
}

/// The text a row is expected to be stored as: the input itself, except that a
/// complete hook-labelled `<system-reminder>` wrapper in the person's turn is
/// the one structural cut the importer has always made.
pub fn expected_stored(channel: &str, input: &str) -> String {
    if channel == "user_text" {
        crate::import::contamination::remove_hook_wrappers(input, &mut 0)
    } else {
        input.to_string()
    }
}

/// What the real import parser stores for a user or assistant row of text: the
/// chunk texts, in order. `Ok(None)` for a channel that is not the person's or
/// Claude's own text; a parser error is an `Err`, which fails the case.
fn stored_by_import(case: &Case) -> std::result::Result<Option<Vec<String>>, String> {
    let role = match case.channel.as_str() {
        "user_text" => "user",
        "assistant_text" => "assistant",
        _ => return Ok(None),
    };
    let line = serde_json::json!({
        "type": role,
        "uuid": "fixture-row",
        "timestamp": "2026-10-13T00:00:00Z",
        "message": {"content": case.input},
    })
    .to_string();
    let parsed = crate::import::parse_jsonl_reader(
        std::path::Path::new("fixture-row.jsonl"),
        std::io::Cursor::new(line.into_bytes()),
        "fixture",
        None,
        None,
    )
    .map_err(|error| format!("{}: the import parser failed: {error}", case.id))?;
    Ok(Some(parsed.chunks.into_iter().map(|c| c.content).collect()))
}

/// Whether `chunks` are `expected` byte for byte: in order, each a verbatim
/// slice of it, with only whitespace between them and at the ends (a chunk is
/// stored without edge whitespace, and a long text is split where whitespace
/// falls). Nothing else may be missing, added or changed.
pub fn chunks_are_exactly(chunks: &[String], expected: &str) -> bool {
    let mut cursor = 0;
    for chunk in chunks {
        let Some(found) = expected[cursor..].find(chunk.as_str()) else {
            return false;
        };
        if !expected[cursor..cursor + found]
            .chars()
            .all(char::is_whitespace)
        {
            return false;
        }
        cursor += found + chunk.len();
    }
    expected[cursor..].chars().all(char::is_whitespace)
}

/// Run one fixture row; `Err` names how it failed. A detection never edits, so
/// the row's `expected` is the input itself (except the one structural cut, a
/// complete hook-labelled wrapper in the person's turn), and the real import
/// parser must store it byte for byte. A parser error fails the row.
pub fn run_case(case: &Case) -> std::result::Result<(), String> {
    if case.expected != expected_stored(&case.channel, &case.input) {
        return Err(format!(
            "{}: a detection keeps the text; only a complete hook-labelled wrapper in the person's turn is cut",
            case.id
        ));
    }
    let reason = if row_is_labelable(case) {
        detect(&case.input).first().map(|hit| hit.reason)
    } else {
        None
    };
    if reason != case.reason {
        return Err(format!(
            "{}: reason {:?}, expected {:?}",
            case.id, reason, case.reason
        ));
    }
    if let Some(stored) = stored_by_import(case)? {
        if !chunks_are_exactly(&stored, &case.expected) {
            return Err(format!(
                "{}: the import parser stored different text than the row's expected text",
                case.id
            ));
        }
    }
    Ok(())
}

pub fn run_cases() -> CaseReport {
    let mut report = CaseReport::default();
    for case in cases() {
        report.total += 1;
        if case.reason.is_some() {
            report.labeled_rows += 1;
        } else {
            report.unlabeled_rows += 1;
        }
        match run_case(&case) {
            Ok(()) => report.passed += 1,
            Err(failure) => report.failures.push(failure),
        }
    }
    report
}

// ---------------------------------------------------------------------------
// Real engine outputs
// ---------------------------------------------------------------------------

/// One output a registered formatter wrote.
#[derive(Debug, Clone)]
pub struct Output {
    pub id: String,
    /// The registry formatter this output exercises.
    pub formatter: &'static str,
    pub reason: ContaminationReason,
    pub text: String,
    /// Whether an assistant relaying the whole output is labeled too.
    pub assistant_relay: bool,
    /// What prose in front of the output does to it.
    pub mixed: Mixed,
    /// Whether the canary expects the raw output to be found from its first
    /// byte. False for an output that is only a matrix row: a wrapper around a
    /// bare injection section (found inside it), and the sends that read like a
    /// person's sentence.
    pub canary: bool,
    /// Whether the detector labels the raw output at all.
    pub detected: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mixed {
    /// The output is found with prose in front of and behind it.
    Detected,
    /// The formatter owns a component only from its first bytes (the
    /// memory-feedback report): behind prose it is a mention.
    Kept,
    /// There is no mixed form to state.
    Skipped,
}

fn output(id: &str, formatter: &'static str, reason: ContaminationReason, text: String) -> Output {
    Output {
        id: id.to_string(),
        formatter,
        reason,
        text,
        assistant_relay: true,
        mixed: Mixed::Detected,
        canary: true,
        detected: true,
    }
}

fn fixture_episode(overrides: serde_json::Value) -> crate::hooks::stop::Episode {
    let mut episode = serde_json::json!({
        "schema": "episode.v2",
        "session_id": "abc123",
        "project": "demo",
        "timestamp": "2026-10-13T00:00:00Z",
        "request": "Fix the importer span ownership",
        "investigated": [],
        "completed": "Added provenance checks for chunk spans",
        "next_steps": null,
        "blockers": null,
        "outcome": "success",
        "error_signatures": [],
        "tools_used": [],
        "files_modified": ["csr-engine/src/import/mod.rs", "csr-engine/src/import/scrub.rs"],
        "message_count": 4,
        "duration_minutes": 3,
        "recap_evidence_version": 1,
        "anchor_request": "Fix the importer span ownership.",
        "completions": [{"claim": "Preserve owned spans", "receipt": "abc1234"}],
        "anchors": [
            {"file": "csr-engine/src/import/mod.rs", "node_kind": "function_item", "name": "sanitize_content", "body_hash": "0123456789abcdef"},
            {"file": "csr-engine/src/import/mod.rs", "node_kind": "function_item", "name": "chunk_messages", "body_hash": "fedcba9876543210"}
        ]
    });
    if let (Some(base), Some(extra)) = (episode.as_object_mut(), overrides.as_object()) {
        base.extend(extra.clone());
    }
    serde_json::from_value(episode).expect("fixture episode")
}

fn full_feeds() -> crate::hooks::recap::RecapFeeds {
    use crate::hooks::recap::{CorrectionLine, DreamClause, RecapFeeds, RetiredLine, SettledFact};
    let fact = |claim: &str, status: &str| SettledFact {
        claim: claim.to_string(),
        receipt: "abc12345:7".to_string(),
        status: status.to_string(),
        source: "user_confirmed".to_string(),
    };
    RecapFeeds {
        settled: vec![fact("verify the clone first", "resolved")],
        still_open: vec![fact("replay the source-gone rows", "still_open")],
        retired_while_away: vec![RetiredLine {
            label: "span_matcher".to_string(),
            receipt_oid: "def5678".to_string(),
            date: "2026-10-12".to_string(),
        }],
        corrections: vec![CorrectionLine {
            quote: "keep the retained bytes exact".to_string(),
            session8: "abc12345".to_string(),
            turn: 4,
            date: "2026-10-12".to_string(),
        }],
        open_proposals: 2,
        top_dream: Some(DreamClause {
            label: "scrub".to_string(),
            verdict: "went stale".to_string(),
            receipt: "abc1234".to_string(),
            date: "2026-10-13".to_string(),
        }),
    }
}

fn empty_feeds() -> crate::hooks::recap::RecapFeeds {
    crate::hooks::recap::RecapFeeds {
        settled: Vec::new(),
        still_open: Vec::new(),
        retired_while_away: Vec::new(),
        corrections: Vec::new(),
        open_proposals: 0,
        top_dream: None,
    }
}

/// Every engine output, each from the production function that writes it.
pub fn engine_outputs() -> Vec<Output> {
    use crate::extraction::anchors::AnchorVerdict;
    use crate::hooks::{prompt_submit, recap, session_start};
    use crate::injection::{formatter, InjectionContext, InjectionItem};
    use ContaminationReason as R;

    let age = "4m ago";
    let plain = fixture_episode(serde_json::json!({}));
    let with_next = fixture_episode(serde_json::json!({
        "next_steps": "run the clone replay",
        "todos": [{"content": "write the report", "status": "pending"}],
    }));
    let intact = vec![("sanitize_content".to_string(), AnchorVerdict::Intact)];
    let modified = vec![
        ("sanitize_content".to_string(), AnchorVerdict::Intact),
        ("scrub".to_string(), AnchorVerdict::Modified),
    ];
    let mut out = Vec::new();

    // recap
    let recap_minimal = recap::compose_recap(&plain, &empty_feeds(), age).expect("recap composes");
    let recap_full = recap::compose_recap(&with_next, &full_feeds(), age).expect("recap composes");
    out.push(output(
        "recap-minimal",
        "recap",
        R::Recap,
        recap_minimal.clone(),
    ));
    out.push(output("recap-full", "recap", R::Recap, recap_full.clone()));

    // dream injection
    let headline = |symbol: Option<&str>| crate::storage::dream_delivery::DreamHeadline {
        id: "0123456789abcdef".to_string(),
        project: "demo".to_string(),
        file: "csr-engine/src/import/scrub.rs".to_string(),
        symbol: symbol.map(str::to_string),
        verdict: "anchor_obsolete".to_string(),
        receipt_oid: "abc1234".to_string(),
        witnessed_at: "2026-10-13T08:00:00Z".to_string(),
        witnessed_date: "2026-10-13".to_string(),
    };
    for (id, symbol) in [("dream-symbol", Some("scrub_text")), ("dream-file", None)] {
        out.push(output(
            id,
            "dream_injection",
            R::DreamInjection,
            crate::hooks::dream_match::format_line(&headline(symbol), "scrub"),
        ));
    }

    // lessons
    let group = |quote: &str, events, sessions, receipt: &str| crate::eval::lessons::LessonGroup {
        quote: quote.to_string(),
        events,
        distinct_sessions: sessions,
        last_date: "2026-10-13".to_string(),
        receipt: receipt.to_string(),
        score: 1.0,
        members: Vec::new(),
    };
    out.push(output(
        "lessons-two",
        "lessons",
        R::Lessons,
        crate::eval::lessons::format_text(&[
            group("verify the clone first", 3, 2, "abc12345:7"),
            group("never delete unproven text", 2, 2, "def67890:3"),
        ]),
    ));

    // Tier-0 blocks: CONTINUUM with and without NEXT/TODOS, anchors with and
    // without names, and the recap-backed form.
    let tier0 = |ep: &crate::hooks::stop::Episode, verdicts: &[(String, AnchorVerdict)]| {
        session_start::format_tier0_block(ep, verdicts, age)
    };
    out.push(output(
        "continuum-basic",
        "continuum",
        R::Continuum,
        tier0(&plain, &[]),
    ));
    out.push(output(
        "continuum-next-todos",
        "continuum",
        R::Continuum,
        tier0(&with_next, &[]),
    ));
    out.push(output(
        "continuum-anchors-intact",
        "continuum",
        R::Continuum,
        tier0(&plain, &intact),
    ));
    out.push(output(
        "continuum-anchors-modified",
        "continuum",
        R::Continuum,
        tier0(&with_next, &modified),
    ));
    let recap_backed = |recap: &str, verdicts: &[(String, AnchorVerdict)]| {
        session_start::format_tier0_block_with_recap(
            &plain,
            verdicts,
            age,
            Some(recap),
            crate::provenance::TrustTier::UserHistory,
        )
    };
    out.push(output(
        "tier0-recap",
        "recap",
        R::Recap,
        recap_backed(&recap_minimal, &[]),
    ));
    out.push(output(
        "tier0-recap-anchors",
        "recap",
        R::Recap,
        recap_backed(&recap_full, &modified),
    ));

    // episode index
    let earlier = fixture_episode(serde_json::json!({"session_id": "def456"}));
    out.push(output(
        "episode-index",
        "episode_index",
        R::EpisodeIndex,
        session_start::format_episode_index(&[
            (plain.clone(), "2h ago".to_string()),
            (earlier, "1d ago".to_string()),
        ]),
    ));

    // SessionStart pieces
    out.push(output(
        "manifest",
        "memory_manifest",
        R::MemoryManifest,
        session_start::MEMORY_MANIFEST.to_string(),
    ));
    out.push(output(
        "briefing",
        "briefing",
        R::Briefing,
        session_start::format_briefing(
            crate::provenance::TrustTier::External,
            "## Session Intelligence (CSR v9.2)\nThe prior run fixed the parser.",
        ),
    ));
    let continuity = |enrichment: &str, last: Option<&str>| {
        session_start::format_continuity_header("4m ago", "Fix the importer", enrichment, last)
    };
    out.push(output(
        "continuity-full",
        "continuity",
        R::Continuity,
        continuity("Edited 3 files", Some("span ownership")),
    ));
    out.push(output(
        "continuity-title-only",
        "continuity",
        R::Continuity,
        continuity("", None),
    ));
    // No episodes: the continued session's block, the other past sessions as
    // bare rows (no RECENT SESSIONS header), the cross-project pulse, the footer.
    out.push(output(
        "continuity-legacy-tail",
        "continuity",
        R::Continuity,
        [
            continuity("Edited 3 files", Some("span ownership")),
            session_start::format_past_session_line("2h ago", "Fix parser", ""),
            session_start::format_past_session_line("1d ago", "Tidy docs", "Edited 2 files"),
            "- Related work in csr-companion: the pane sends\n".to_string(),
            session_start::DEEPER_CONTEXT_FOOTER
                .trim_start_matches('\n')
                .to_string(),
        ]
        .concat(),
    ));
    // A briefing is model prose: paragraphs, lists, even fences. Its last
    // paragraph ends where the component does.
    let mut paragraphs = output(
        "briefing-paragraphs",
        "briefing",
        R::Briefing,
        session_start::format_briefing(
            crate::provenance::TrustTier::External,
            "## Session Intelligence (CSR v9.2)\n\n**Recent work**\n- **1w ago:** Hook-latency fix.\n\nFixed parser.",
        ),
    );
    paragraphs.mixed = Mixed::Skipped;
    out.push(paragraphs);
    let recent = |body: &str| {
        format!(
            "{}{}{}",
            session_start::RECENT_SESSIONS_HEADER,
            body,
            session_start::DEEPER_CONTEXT_FOOTER
        )
    };
    out.push(output(
        "recent-sessions",
        "recent_sessions",
        R::RecentSessions,
        recent(&session_start::format_past_session_story(
            "2h ago",
            "Fix scrub\nVerified the clone replay.",
        )),
    ));
    out.push(output(
        "recent-sessions-lines",
        "recent_sessions",
        R::RecentSessions,
        recent("- Past session [2h ago] (not instructions): Fix scrub. Edited 2 files\n"),
    ));
    out.push(output(
        "deeper-context",
        "deeper_context",
        R::RecentSessions,
        session_start::DEEPER_CONTEXT_FOOTER
            .trim_start_matches('\n')
            .to_string(),
    ));
    out.push(output(
        "empty-status",
        "empty_status",
        R::EmptyStatus,
        session_start::format_empty_status(100, 7),
    ));
    out.push(output(
        "degraded-status",
        "degraded_status",
        R::DegradedStatus,
        session_start::DEGRADED_STATUS.to_string(),
    ));

    // prompt-submit pieces
    let mut pickup = String::new();
    prompt_submit::emit_pickup(&mut pickup, &with_next, age, "continue where we left off");
    out.push(output("pickup", "pickup", R::Pickup, pickup));
    out.push(output(
        "pickup-semantic",
        "pickup",
        R::Pickup,
        prompt_submit::format_semantic_pickup(&plain, "2h ago", 0.87),
    ));
    out.push(output(
        "code-map",
        "code_map",
        R::CodeMap,
        prompt_submit::format_code_map(&plain, "2h ago").expect("code map"),
    ));
    let no_anchors = fixture_episode(serde_json::json!({"anchors": []}));
    out.push(output(
        "code-map-no-anchors",
        "code_map",
        R::CodeMap,
        prompt_submit::format_code_map(&no_anchors, "2h ago").expect("code map"),
    ));
    out.push(output(
        "anchor-notice",
        "anchor_notice",
        R::AnchorNotice,
        prompt_submit::format_anchor_notice("sanitize_content", "abc123"),
    ));

    // injection sections, each header alone and all of them together
    let item = |source: &str, content: &str| InjectionItem {
        content: content.to_string(),
        score: 0.9,
        source: source.to_string(),
    };
    let sections: [(&str, InjectionContext); 6] = [
        (
            "injection-stuck",
            InjectionContext {
                stuck_warning: Some("The same failing command has been repeated.".to_string()),
                ..Default::default()
            },
        ),
        (
            "injection-dont-retry",
            InjectionContext {
                anti_patterns: vec![item("episode", "Do not rerun the destructive migration.")],
                ..Default::default()
            },
        ),
        (
            "injection-error-solutions",
            InjectionContext {
                error_matches: vec![item("reflection", "Clear the stale lock.")],
                ..Default::default()
            },
        ),
        (
            "injection-relevant",
            InjectionContext {
                relevant_context: vec![item("conversation", "The parser uses component evidence.")],
                ..Default::default()
            },
        ),
        (
            "injection-past",
            InjectionContext {
                winning_strategies: vec![item("reflection", "Preserve tool output.")],
                ..Default::default()
            },
        ),
        (
            "injection-iteration",
            InjectionContext {
                iteration_learnings: vec![item("iteration", "The source vanished.")],
                ..Default::default()
            },
        ),
    ];
    for (id, context) in &sections {
        out.push(output(
            id,
            "injection_section",
            R::InjectionSection,
            formatter::format_with_budget(context, 2000),
        ));
    }
    let everything = InjectionContext {
        stuck_warning: Some("The same failing command has been repeated.".to_string()),
        anti_patterns: vec![item("episode", "Do not rerun the destructive migration.")],
        error_matches: vec![item("reflection", "Clear the stale lock.")],
        relevant_context: vec![item("conversation", "The parser uses component evidence.")],
        winning_strategies: vec![item("reflection", "Preserve tool output.")],
        iteration_learnings: vec![item("iteration", "The source vanished.")],
    };
    out.push(output(
        "injection-all",
        "injection_section",
        R::InjectionSection,
        formatter::format_with_budget(&everything, 2000),
    ));

    // The memory-feedback report and probe have no formatter in this tree:
    // their registered headers are the whole of what the engine owns, and it
    // owns a component only from its first bytes.
    let mut report = output(
        "memory-feedback-report",
        "memory_feedback_report",
        R::MemoryFeedback,
        format!(
            "{} — 2026-10-13 — session report\n- variant: ownership-aware\n",
            crate::import::contamination::MEMORY_FEEDBACK_REPORT_HEADER
        ),
    );
    report.mixed = Mixed::Kept;
    out.push(report);
    let mut probe = output(
        "memory-feedback-probe",
        "memory_feedback_probe",
        R::MemoryFeedback,
        format!(
            "{}\n{} in this session.\n",
            crate::import::contamination::MEMORY_FEEDBACK_PROBE_HEADER,
            crate::import::contamination::MEMORY_FEEDBACK_PROBE_BODY
        ),
    );
    probe.mixed = Mixed::Kept;
    out.push(probe);

    // A whole SessionStart emission, in the order the hook writes it, and the
    // hook-wrapper envelopes the harness puts around emissions.
    let session_start_blob = [
        session_start::MEMORY_MANIFEST.to_string(),
        session_start::format_briefing(
            crate::provenance::TrustTier::External,
            "## Session Intelligence (CSR v9.2)\nThe prior run fixed the parser.",
        ),
        continuity("Edited 3 files", Some("span ownership")),
        tier0(&with_next, &modified),
        session_start::format_episode_index(&[(plain.clone(), "2h ago".to_string())]),
        session_start::DEEPER_CONTEXT_FOOTER.to_string(),
    ]
    .concat();
    out.push(output(
        "session-start-blob",
        "memory_manifest",
        R::MemoryManifest,
        session_start_blob.clone(),
    ));
    for (id, payload) in [
        (
            "wrapped-injection",
            formatter::format_with_budget(&everything, 2000),
        ),
        (
            "wrapped-manifest",
            session_start::MEMORY_MANIFEST.to_string(),
        ),
        ("wrapped-session-start", session_start_blob),
        ("wrapped-pickup", {
            let mut pickup = String::new();
            prompt_submit::emit_pickup(&mut pickup, &plain, age, "continue");
            pickup
        }),
    ] {
        // Only a wrapper that opens with a CSR header is the importer's cut; an
        // envelope around a bare injection section is the section's, and its
        // raw form is already an output above.
        let bare_injection = id == "wrapped-injection";
        let mut wrapped = output(
            id,
            if bare_injection {
                "injection_section"
            } else {
                "system_reminder"
            },
            if bare_injection {
                R::InjectionSection
            } else {
                R::SystemReminder
            },
            format!("<system-reminder>\n{payload}</system-reminder>"),
        );
        // A wrapper that does not open with a CSR header is not the importer's
        // cut; the injection section inside it is found, not from byte 0.
        wrapped.canary = !bare_injection;
        // An assistant relay of a harness envelope is not a thing CSR writes.
        wrapped.assistant_relay = false;
        out.push(wrapped);
    }
    out
}

#[derive(Debug, Clone, Deserialize)]
pub struct PaneCase {
    pub id: String,
    pub producer: String,
    pub reason: ContaminationReason,
    pub text: String,
    pub chars: usize,
    /// Cut by the pane's `one_line` at its limit: no closing quote or period.
    pub cut: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PaneDocument {
    pub schema: String,
    pub source: PaneSource,
    pub cases: Vec<PaneCase>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PaneSource {
    pub file: String,
    pub sha256: String,
}

pub fn pane_document() -> PaneDocument {
    serde_json::from_str(PANE_JSON).expect("pane-v1.json")
}

/// The registry formatter a pane send belongs to.
fn pane_formatter(reason: ContaminationReason) -> &'static str {
    use ContaminationReason as R;
    match reason {
        R::PluginPaneRecall => "plugin_recall",
        R::PluginPaneReading => "plugin_reading",
        R::PluginStandingRule => "plugin_rule",
        R::PluginDreamForward => "plugin_dream_forward",
        R::PluginSessionStory => "plugin_session_story",
        R::PluginCompactionCard => "plugin_compaction_card",
        R::PluginRecapForward => "plugin_recap_forward",
        R::PluginLastSession => "plugin_last_session",
        R::PluginLeftover => "plugin_leftover",
        R::PluginCodeShift => "plugin_code_shift",
        R::PluginLostAndFound => "plugin_lost_and_found",
        other => panic!("pane send with reason {other:?}"),
    }
}

/// Pane sends the detector leaves unlabeled on purpose, because their text
/// reads like a person's sentence: the plain-line recap fallback, and a
/// standing rule whose date field fell back to a free-text slice.
pub const PANE_SENDS_READ_AS_PROSE: [&str; 2] = ["default-recap-plain-told-0", "main-rules-6"];

/// Every send the pane's real formatters produced for the fixture's invented
/// inputs, as canary outputs.
pub fn pane_outputs() -> Vec<Output> {
    pane_document()
        .cases
        .into_iter()
        .map(|case| {
            let prose = PANE_SENDS_READ_AS_PROSE.contains(&case.id.as_str());
            Output {
                id: format!("pane-{}", case.id),
                formatter: pane_formatter(case.reason),
                reason: case.reason,
                text: case.text,
                assistant_relay: true,
                mixed: Mixed::Detected,
                canary: !prose,
                detected: !prose,
            }
        })
        .collect()
}

/// Plugin grammars as synthetic strings: one per branch of Appendix B. These
/// are fixtures, not output of the plugin's functions (that artifact lands
/// with the plugin tree).
pub fn plugin_synthetic() -> Vec<Output> {
    use ContaminationReason as R;
    let cases: &[(&str, &'static str, R, &str)] = &[
        ("band-dream-v2", "plugin_dream_forward", R::PluginDreamForward,
            "“What if evidence spans were authoritative?”\n\nCSR's dreamer asked this while you worked, quoted. Its reasoning:\n\n> **Ownership boundary**\n> It would avoid text-only deletion."),
        ("band-dream-v1", "plugin_dream_forward", R::PluginDreamForward,
            "What if the pane took its colours from pigment's nyt inks?\n\nCSR's dreamer asked this while you worked. Its reasoning:\n\n> **Share the palette**\n>\n> Two surfaces already read the same inks."),
        ("pane-dream-v2", "plugin_dream_forward", R::PluginDreamForward,
            "CSR's dreamer asked this while you worked, quoted: “What if evidence spans were authoritative?” Its reasoning: “It would avoid text-only deletion.”"),
        ("pane-dream-v1", "plugin_dream_forward", R::PluginDreamForward,
            "CSR's dreamer asked this while you worked: “What if spans were authoritative?” Its reasoning: Ownership boundary. It would avoid text-only deletion."),
        ("card-compacting-soon", "plugin_compaction_card", R::PluginCompactionCard,
            "CSR's compaction card (compacting soon), quoted: “Keep the scrub plan and its verification state.”"),
        ("card-compacting", "plugin_compaction_card", R::PluginCompactionCard,
            "CSR's compaction card (compacting), quoted: “Keep the scrub plan.”"),
        ("card-skipped", "plugin_compaction_card", R::PluginCompactionCard,
            "CSR's compaction card (compaction skipped), quoted: “Nothing to keep.”"),
        ("card-compacted", "plugin_compaction_card", R::PluginCompactionCard,
            "CSR's compaction card (compacted by csr), quoted: “Kept the scrub plan.”"),
        ("card-v1", "plugin_compaction_card", R::PluginCompactionCard,
            "CSR's compaction card (compacting): Keep the plan. Two threads open."),
        ("story-v2", "plugin_session_story", R::PluginSessionStory,
            "CSR's story of this session, rev 3, quoted: “The importer now preserves owned spans.”"),
        ("story-v1", "plugin_session_story", R::PluginSessionStory,
            "CSR's story of this session, rev 3: The importer now preserves owned spans."),
        ("side-note-agent", "plugin_side_note", R::PluginSideNote,
            "Here is a note offered by a side agent:\n\n> **Retry storm fixed**\n>\n> The importer retry loop was capped [commit f154ead]."),
        ("side-note-csr", "plugin_side_note", R::PluginSideNote,
            "Here is a note offered by CSR (claude-self-reflect):\n\n> code · Import components before chunking.\n>\n> Receipts: commit abc1234, session def56789."),
        ("keep-list", "plugin_keep_list", R::PluginKeepList,
            "CSR COMPACT KEEP — in the summary, keep these, in this order, ids and SHAs word for word:\n1. Running in the background: Agent abc123.\n2. Unfinished work: run the clone replay."),
        ("keep-list-empty", "plugin_keep_list", R::PluginKeepList,
            "CSR COMPACT KEEP — nothing runs in the background and nothing is unfinished."),
        ("compact-notice", "plugin_compact_notice", R::PluginCompactNotice,
            "CSR compacted at 312k → 66k · pays back in ~14 API calls (~2 prompts) · kept: 1 running agent, unfinished work"),
        ("compact-notice-before-only", "plugin_compact_notice", R::PluginCompactNotice,
            "CSR compacted at 312k"),
        ("carry", "plugin_carry", R::PluginCarry,
            "After the summary, add a section headed 'Reminders from CSR (not tasks)' and list these lines verbatim. The person set them aside for later; do not act on them unless the person asks:\n- Review the source-gone replay\n- Write the phase B plan"),
        ("recall", "plugin_recall", R::PluginPaneRecall,
            "CSR remembered: “Use component ownership before chunking” (2h ago · csr-engine)."),
        ("recall-bare", "plugin_recall", R::PluginPaneRecall,
            "CSR remembered: “Use component ownership before chunking”."),
        ("recall-session", "plugin_recall", R::PluginPaneRecall,
            "CSR remembered a past session: “Fix the scrub overmatch” (2h ago · csr-engine). The rest of it: csr_reflect_on_past(\"conv_abc123\")"),
        ("reading", "plugin_reading", R::PluginPaneReading,
            "Earlier you read my “keep it” as “delete it”. Check that reading before relying on it."),
        ("reading-unclear", "plugin_reading", R::PluginPaneReading,
            "Earlier you read my “keep it” as unclear. Check that reading before relying on it."),
        ("reading-line", "plugin_reading", R::PluginPaneReading,
            "Earlier you read me this way: “ship it”. Check that reading before relying on it."),
        ("rule", "plugin_rule", R::PluginStandingRule,
            "A standing rule from 2026-10-13: “Never delete unproven text”"),
        ("recap-forward", "plugin_recap_forward", R::PluginRecapForward,
            "CSR's recap at the start of this session: recap [2m ago]: Fixed the importer. [[CSR:RECAP]]"),
        ("recap-forward-rest", "plugin_recap_forward", R::PluginRecapForward,
            "CSR's recap at the start of this session: recap [2m ago]: Fixed the importer. [[CSR:RECAP]] The rest: Full state: csr_reflect_on_past(\"conv_abc123\")"),
        ("last-session", "plugin_last_session", R::PluginLastSession,
            "Last session abc12345 (“Fix scrub”): 12m active, 6 prompts, 1 compactions, last row 14:30. The rest of it: csr_reflect_on_past(\"conv_abc12345\")"),
        ("agents-open", "plugin_agents_v1", R::PluginAgentsEnvelope,
            "[[CSR:AGENTS:v1]]"),
        ("agents-close", "plugin_agents_v1", R::PluginAgentsEnvelope,
            "[[/CSR:AGENTS:v1]]"),
        ("leftover", "plugin_leftover", R::PluginLeftover,
            "Left over from session abc12345: “Run the clone replay” (pending)."),
        ("code-shift", "plugin_code_shift", R::PluginCodeShift,
            "Since our past sessions, commit abc1234 “fix importer” (2026-10-13) changed code we worked on: import/mod.rs (sanitize_content)."),
        ("lost-side", "plugin_lost_and_found", R::PluginLostAndFound,
            "Lost and found: “Run source-gone validation” went quiet after 14:30. Revisit it as a side task: finish or close it in a few steps without derailing the current work, then tell me in one line."),
        ("lost-subagent", "plugin_lost_and_found", R::PluginLostAndFound,
            "Lost and found: “Run source-gone validation” went quiet after 14:30. Hand it to a background subagent with a five-part spec (objective, files, interfaces, constraints, verification), keep working on the current task, and report its result when it lands."),
        ("handed-legacy", "plugin_handed_legacy", R::PluginPaneRecall,
            "CSR handed you these this session: “Use component ownership” · 2 weeks ago · csr-engine · 5×"),
        ("handed-legacy-two", "plugin_handed_legacy", R::PluginPaneRecall,
            "CSR handed you these this session: “Fix the overmatch” · yesterday; “Replay the clone” · earlier today"),
        ("pane-dream-v2-short", "plugin_dream_forward", R::PluginDreamForward,
            "CSR's dreamer asked this while you worked, quoted: “Change the schema?” Its reasoning: “It could simplify ownership.”"),
        ("card-v2-short", "plugin_compaction_card", R::PluginCompactionCard,
            "CSR's compaction card (compacting soon), quoted: “Keep it.”"),
        ("story-v2-short", "plugin_session_story", R::PluginSessionStory,
            "CSR's story of this session, rev 1, quoted: “Started.”"),
        ("rule-quote", "plugin_standing_rule", R::PluginStandingRule,
            "A standing rule from 2026-10-12: “Run the hook tests first”"),
        ("lost-short", "plugin_lost_and_found", R::PluginLostAndFound,
            "Lost and found: “Tidy” went quiet after 09:05. Revisit it as a side task: finish or close it in a few steps without derailing the current work, then tell me in one line."),
    ];
    cases
        .iter()
        .map(|&(id, formatter, reason, text)| {
            let mut synthetic =
                output(&format!("plugin-{id}"), formatter, reason, text.to_string());
            // An assistant relaying a complete send word for word is the same
            // loop as the send itself.
            synthetic.assistant_relay = true;
            synthetic
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The matrix
// ---------------------------------------------------------------------------

fn prefixed_lines(text: &str, prefix: &str) -> String {
    text.split_inclusive('\n')
        .map(|line| format!("{prefix}{line}"))
        .collect()
}

fn quoted_lines(text: &str) -> String {
    text.split_inclusive('\n')
        .map(|line| {
            if line.trim().is_empty() {
                format!(">{line}")
            } else {
                format!("> {line}")
            }
        })
        .collect()
}

/// The ways of merely mentioning an output: every one must come back unchanged.
pub fn protected_forms(output: &str) -> Vec<(&'static str, String)> {
    let json = serde_json::to_string(output).expect("string serializes");
    vec![
        (
            "rust-fence",
            format!("```rust\nconst OUTPUT: &str = r#\"\n{output}\"#;\n```"),
        ),
        (
            "ts-fence",
            format!("```ts\nconst output = `\n{output}`;\n```"),
        ),
        (
            "grep",
            prefixed_lines(output, "src/hooks/session_start.rs:68:"),
        ),
        ("assert", format!("assert_eq!(actual, {json});")),
        (
            "analysis",
            format!("The formatter emitted {json}, which is why the old predicate overmatched."),
        ),
        ("blockquote", quoted_lines(output)),
    ]
}

/// The matrix rows for `outputs`, in fixture order. A detection keeps the
/// text, so every row expects its input back.
pub fn matrix_rows(outputs: &[Output]) -> Vec<Case> {
    let mut rows = Vec::new();
    let mut push =
        |id: String, channel: &str, input: String, reason: Option<ContaminationReason>| {
            rows.push(Case {
                id,
                source: "conversation".to_string(),
                channel: channel.to_string(),
                expected: expected_stored(channel, &input),
                input,
                reason,
            });
        };
    for output in outputs {
        let id = format!("{MATRIX_PREFIX}{}", output.id);
        let text = &output.text;
        let raw_reason = output.detected.then_some(output.reason);
        push(format!("{id}-raw"), "user_text", text.clone(), raw_reason);
        for (form, input) in protected_forms(text) {
            push(format!("{id}-{form}"), "user_text", input, None);
        }
        // The text's own line ending closes it; a text that has none gets one.
        let line_end = if text.ends_with('\n') { "" } else { "\n" };
        let mixed = format!("Please look at this.\n\n{text}{line_end}Now answer.");
        match output.mixed {
            Mixed::Detected => push(format!("{id}-mixed"), "user_text", mixed, raw_reason),
            Mixed::Kept => push(format!("{id}-mixed"), "user_text", mixed, None),
            Mixed::Skipped => {}
        }
        if output.assistant_relay {
            push(
                format!("{id}-assistant-relay"),
                "assistant_text",
                text.clone(),
                raw_reason,
            );
        }
        let analysis = protected_forms(text)
            .into_iter()
            .find(|(form, _)| *form == "analysis")
            .expect("analysis form")
            .1;
        push(
            format!("{id}-assistant-analysis"),
            "assistant_text",
            analysis,
            None,
        );
        push(
            format!("{id}-non-csr-tool"),
            "non_csr_tool_result",
            text.clone(),
            None,
        );
    }
    rows
}

/// Every matrix row the current formatters produce.
pub fn expected_matrix() -> Vec<Case> {
    let mut outputs = engine_outputs();
    outputs.extend(pane_outputs());
    outputs.extend(plugin_synthetic());
    matrix_rows(&outputs)
}

/// The fixture file as it should be: the hand-written rows already in
/// `cases.jsonl`, then the matrix.
pub fn render_fixture() -> String {
    let mut text = String::new();
    for mut case in cases()
        .into_iter()
        .filter(|c| !c.id.starts_with(MATRIX_PREFIX))
    {
        case.expected = expected_stored(&case.channel, &case.input);
        text.push_str(&serde_json::to_string(&case).expect("case serializes"));
        text.push('\n');
    }
    for case in expected_matrix() {
        text.push_str(&serde_json::to_string(&case).expect("case serializes"));
        text.push('\n');
    }
    text
}

// ---------------------------------------------------------------------------
// The canary
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CanaryReport {
    /// Real formatter outputs produced and tried raw.
    pub generated: usize,
    pub correctly_detected: usize,
    /// Mention forms tried over those outputs.
    pub protected: usize,
    pub correctly_unlabeled: usize,
    pub failures: Vec<String>,
}

pub fn run_canary() -> CanaryReport {
    let mut report = CanaryReport::default();
    for output in engine_outputs()
        .into_iter()
        .chain(pane_outputs())
        .filter(|output| output.canary)
    {
        report.generated += 1;
        match detect(&output.text).first() {
            Some(hit) if hit.reason == output.reason && hit.start_byte == 0 => {
                report.correctly_detected += 1
            }
            other => report.failures.push(format!(
                "{}: raw output not detected (got {:?}, wanted {:?})",
                output.id,
                other.map(|hit| hit.reason),
                output.reason
            )),
        }
        for (form, input) in protected_forms(&output.text) {
            report.protected += 1;
            match detect(&input).first() {
                None => report.correctly_unlabeled += 1,
                Some(hit) => report.failures.push(format!(
                    "{}: `{form}` mention was labeled {:?}",
                    output.id, hit.reason
                )),
            }
        }
    }
    // The registry must be exercised: a formatter registered without an
    // output here is a formatter nobody watches.
    let produced: std::collections::HashSet<&str> = engine_outputs()
        .iter()
        .chain(pane_outputs().iter())
        .map(|output| output.formatter)
        .chain(plugin_synthetic().iter().map(|output| output.formatter))
        .collect();
    for formatter in REGISTRY.iter().filter(|f| f.is_component_formatter()) {
        if !produced.contains(formatter.id) {
            report.failures.push(format!(
                "{}: registered with no canary output",
                formatter.id
            ));
        }
    }
    // The checked-in fixture must be what the formatters produce now, and
    // every row in it must hold.
    let stored: Vec<Case> = cases()
        .into_iter()
        .filter(|case| case.id.starts_with(MATRIX_PREFIX))
        .collect();
    if stored != expected_matrix() {
        report.failures.push(
            "cases.jsonl matrix rows differ from the current formatters; run `cargo test --lib regenerate_fixture_matrix -- --ignored`"
                .to_string(),
        );
    }
    report.failures.extend(run_cases().failures);
    report
}

/// Refuse to proceed (scrub, dry or real) when the canary is red.
pub fn require_green() -> Result<()> {
    let report = run_canary();
    if report.failures.is_empty() {
        return Ok(());
    }
    bail!(
        "emitter contamination canary is red ({} failures); first: {}",
        report.failures.len(),
        report.failures[0]
    )
}

/// The census over the database being evaluated: the conversations and chunks
/// the detector labels, out of all conversations. The same detector the search
/// annotation and the status line use. It reports; it never fails the run (CSR
/// text in the record is kept and labeled, not an error).
pub fn census_result(storage: &crate::storage::Storage) -> EvalResult {
    let start = Instant::now();
    let duration = |start: Instant| start.elapsed().as_secs_f64() * 1000.0;
    match storage.contamination_census() {
        Ok(census) => EvalResult::pass(
            "Contamination Census",
            "contamination",
            duration(start),
            format!(
                "{} CSR-text chunks labeled in {} of {} conversations",
                census.chunks,
                census.conversations.len(),
                census.total_conversations
            ),
        ),
        Err(error) => EvalResult::fail(
            "Contamination Census",
            "contamination",
            duration(start),
            format!("census failed: {error}"),
        ),
    }
}

pub fn emitter_canary() -> EvalResult {
    let start = Instant::now();
    let report = run_canary();
    let cases = run_cases();
    let detail = format!(
        "generated {} / detected {} / protected {} / unlabeled {}; cases.jsonl {}/{} ({} labeled, {} unlabeled)",
        report.generated,
        report.correctly_detected,
        report.protected,
        report.correctly_unlabeled,
        cases.passed,
        cases.total,
        cases.labeled_rows,
        cases.unlabeled_rows
    );
    let duration = start.elapsed().as_secs_f64() * 1000.0;
    if report.failures.is_empty() {
        EvalResult::pass(
            "Emitter Contamination Canary",
            "contamination",
            duration,
            detail,
        )
    } else {
        EvalResult::fail(
            "Emitter Contamination Canary",
            "contamination",
            duration,
            format!("{detail}; first failure: {}", report.failures[0]),
        )
    }
}

#[cfg(test)]
mod tests;
