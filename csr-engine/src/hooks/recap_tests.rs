use super::*;
use crate::extraction::provenance::{extractable, RECAP_SENTINEL};
use crate::hooks::stop::{CompletionReceipt, Episode, TodoItem};

fn episode() -> Episode {
    Episode {
        schema: "session_episode/v2".into(),
        session_id: "session-1".into(),
        project: "csr".into(),
        timestamp: "2026-08-07T10:00:00Z".into(),
        request: "Fix\nthe recap composer".into(),
        investigated: vec![],
        completed: "Implemented deterministic recap output".into(),
        next_steps: Some("Run the integration suite".into()),
        blockers: Some("waiting for storage migration".into()),
        outcome: "partial".into(),
        error_signatures: vec![],
        tools_used: vec![],
        files_modified: vec![],
        message_count: 4,
        duration_minutes: 12,
        todos: vec![
            TodoItem {
                content: "wire session start".into(),
                status: "pending".into(),
            },
            TodoItem {
                content: "write composer".into(),
                status: "completed".into(),
            },
        ],
        approved_plan: None,
        prev_episode_id: None,
        error_count: None,
        top_errors: vec![],
        steer_count: None,
        steers: vec![],
        instrumentation_version: None,
        correction_count: None,
        recap_evidence_version: Some(crate::hooks::stop::RECAP_EVIDENCE_VERSION),
        anchor_request: Some("Fix the recap composer".into()),
        completions: vec![CompletionReceipt {
            claim: "Implemented deterministic recap output".into(),
            receipt: "abc1234".into(),
        }],
        completions_untied: false,
        anchor_receipts: None,
        compact_anchor: None,
        late_errors: false,
        closing_success: false,
        closing_ended: false,
        final_failed: false,
        closing_receipt: false,
        outcome_version: None,
        latest_ask: None,
        watermark: None,
        recap_extras: Default::default(),
        anchors: vec![],
    }
}

// ---- session evidence clauses (hooks::recap_evidence) ----

use crate::hooks::recap_evidence::{BackgroundTask, GateFailure};

fn bare_feeds() -> RecapFeeds {
    RecapFeeds {
        settled: vec![],
        still_open: vec![],
        retired_while_away: vec![],
        corrections: vec![],
        open_proposals: 0,
        native_warn: None,
        top_dream: None,
    }
}

/// An episode with an intent and nothing else: no completions, todos,
/// blockers or extras. Every clause below must come from what a test adds.
fn bare_episode() -> Episode {
    let mut ep = episode();
    ep.completions.clear();
    ep.todos.clear();
    ep.next_steps = None;
    ep.blockers = None;
    ep.anchor_receipts = Some(vec![]);
    ep.completions_untied = false;
    ep
}

fn receipt(claim: &str, sha: &str) -> CompletionReceipt {
    CompletionReceipt {
        claim: claim.into(),
        receipt: sha.into(),
    }
}

fn lines_of(text: &str) -> Vec<&str> {
    text.lines().collect()
}

fn full_feeds() -> RecapFeeds {
    RecapFeeds {
        settled: vec![
            SettledFact {
                claim: "schema is stable".into(),
                receipt: "810283b".into(),
                status: "resolved".into(),
                source: "user_confirmed".into(),
            },
            SettledFact {
                claim: "queries are indexed".into(),
                receipt: "T2 2026-07-27".into(),
                status: "resolved".into(),
                source: "user_confirmed".into(),
            },
        ],
        still_open: vec![SettledFact {
            claim: "Windows path behavior".into(),
            receipt: "91abcde".into(),
            status: "still_open".into(),
            source: "user_confirmed".into(),
        }],
        retired_while_away: vec![RetiredLine {
            label: "old schema assumption".into(),
            receipt_oid: "77fedca".into(),
            date: "2026-08-06".into(),
        }],
        corrections: vec![],
        open_proposals: 2,
        native_warn: None,
        top_dream: None,
    }
}

impl RecapFeeds {
    fn empty() -> Self {
        Self {
            settled: vec![],
            still_open: vec![],
            retired_while_away: vec![],
            corrections: vec![],
            open_proposals: 0,
            native_warn: None,
            top_dream: None,
        }
    }
}

// ---- the native-window warning clause ------------------------------

const NATIVE_LINE: &str = "Claude's 300k auto-compact window beat CSR 2\u{d7} this week. \
         Remove autoCompactWindow from settings.json to let CSR lead.";

// ---- Journal v4 P5: the dream clause (delivery channel (b)) ----------

fn dream() -> DreamClause {
    DreamClause {
        label: "parse_config in src/config.rs".into(),
        verdict: "went stale".into(),
        receipt: "abc1234".into(),
        date: "2026-08-11".into(),
        file: "src/config.rs".into(),
        symbol: Some("parse_config".into()),
    }
}

// ---- T3: machine-owned recap sentinel — metamorphic wrapper tests ----
//
// These compose a REAL recap via the canonical producer (not a
// hand-built string) so coverage tracks what actually ships, then apply
// each adversarial wrapper transformation and assert the full
// extractable() pipeline still rejects the result. Non-tautological:
// this exercises extractable()'s prose-extraction grammar end to end,
// not is_csr_emission alone on the composer's raw output.

fn wrap_bullet(text: &str) -> String {
    text.lines()
        .map(|l| format!("- {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn wrap_blockquote(text: &str) -> String {
    text.lines()
        .map(|l| format!("> {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn wrap_emphasis(text: &str) -> String {
    format!("**{text}**")
}

fn wrap_html(text: &str) -> String {
    format!("<div class=\"recap\"><p>{text}</p></div>")
}

fn wrap_zero_width(text: &str) -> String {
    text.chars().flat_map(|c| [c, '\u{200B}']).collect()
}

fn wrap_confusable_brackets(text: &str) -> String {
    text.replace('[', "\u{FF3B}").replace(']', "\u{FF3D}")
}

fn wrap_long_preamble(text: &str) -> String {
    format!(
        "{}\n{text}",
        "filler text that reads like real prose ".repeat(40)
    )
}

// ---- round 5: clause cut for an over-long first task ----

const REYCITAL_1: &str = "This ia new Reycital challenge - there are two videos here /Users/someone/Downloads/IMG_8550.mov /Users/someone/Downloads/IMG_8546.mov - 8550 is the melodica, which is the primary melody and 8546 is the piano - your mission if you chose to take it on is to import both into logic get the bounce out - then for the video propose your thoughts. do initia probes and tell me any questions on concept";
const SELF_BRANDING_1: &str = "I am visualizing a atlantic style essay about how ai usage trends have changed and why even though things look different they are thes same, this is a research and extrapolation task. the eassay looks at what has truly materially changed";

// ---- batch 4a: the recap a reviewer reads ----

const FIELD_PROMPT: &str =
    "Review previous sesison - create a simple unwall for me where we are and how we get to 10.1.";

fn session_start(ep: &Episode, feeds: &RecapFeeds) -> Option<String> {
    compose_recap_with_deliveries(ep, feeds, "3d ago").map(|composed| composed.text)
}

fn witness_dream(label: &str, file: &str, symbol: &str) -> DreamClause {
    DreamClause {
        label: label.into(),
        verdict: "went stale".into(),
        receipt: "abc1234".into(),
        date: "2026-08-11".into(),
        file: file.into(),
        symbol: Some(symbol.into()),
    }
}

fn off_branch(label: &str, receipts: &[&str]) -> RecapContext {
    RecapContext {
        off_head: receipts.iter().map(|r| r.to_string()).collect(),
        head_label: label.into(),
        session_is_live: false,
    }
}

#[path = "recap_tests/mixed_tests.rs"]
mod mixed_tests;

#[path = "recap_tests/composed_recap_tests.rs"]
mod composed_recap_tests;
