//! Prompt injection diagnostics without prompt text.

use crate::injection::predictor::{ScoredResult, Signal};
use crate::telemetry::timing_scrub::{load_salt, query_fields};

/// Renderer-owned decision, carried only by a completed daemon response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DecisionReason {
    OnTopic,
    LookalikeOnly,
    BelowFloor,
    /// The send gate's score cleared its floor but too few items rendered.
    ThinPool,
    NoCandidates,
    TooShort,
    /// The prompt was written by the harness or another agent, not a person.
    MachinePrompt,
    /// Candidates were found, but none of a kind the automatic block carries.
    KindFiltered,
    /// Every candidate left was harness text or boilerplate.
    BoilerplateOnly,
    /// The block would have rested on keyword-only hits, with no vector hit.
    KeywordOnly,
    /// The pool reached its minimum only through items this session already received.
    AlreadyDelivered,
    /// The payload named no session, so the session's own work could not be left out.
    NoSession,
}

impl DecisionReason {
    /// The `reason=` word of the timing log, the serde name.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::OnTopic => "on_topic",
            Self::LookalikeOnly => "lookalike_only",
            Self::BelowFloor => "below_floor",
            Self::ThinPool => "thin_pool",
            Self::NoCandidates => "no_candidates",
            Self::TooShort => "too_short",
            Self::MachinePrompt => "machine_prompt",
            Self::KindFiltered => "kind_filtered",
            Self::BoilerplateOnly => "boilerplate_only",
            Self::KeywordOnly => "keyword_only",
            Self::AlreadyDelivered => "already_delivered",
            Self::NoSession => "no_session",
        }
    }
}

pub(crate) fn log_injection_detail(
    hook: &str,
    query: &str,
    total_items: usize,
    anti_count: usize,
    stdout_bytes: usize,
    scored: &[ScoredResult],
) {
    let disabled = std::env::var("CSR_NO_TIMING_DETAIL").ok();
    if disabled.as_deref() == Some("1") {
        return;
    }
    let Some(path) = crate::telemetry::default_log_path() else {
        return;
    };
    let Ok(salt) = load_salt(&path) else {
        return;
    };
    if let Some(line) = detail_line(
        disabled.as_deref(),
        &salt,
        hook,
        query,
        total_items,
        anti_count,
        stdout_bytes,
        scored,
    ) {
        crate::telemetry::append_timing_line(&line);
    }
}

#[allow(clippy::too_many_arguments)]
fn detail_line(
    disabled: Option<&str>,
    salt: &[u8],
    hook: &str,
    query: &str,
    total_items: usize,
    anti_count: usize,
    stdout_bytes: usize,
    scored: &[ScoredResult],
) -> Option<String> {
    if disabled == Some("1") {
        return None;
    }
    let top_scores: Vec<String> = scored
        .iter()
        .take(3)
        .map(|s| format!("{:.3}/{}", s.final_score, s.source))
        .collect();
    let ancestry_candidates = scored
        .iter()
        .filter(|result| {
            result
                .signals
                .iter()
                .any(|signal| matches!(signal, Signal::AncestryDecay(_)))
        })
        .count();
    Some(format!(
        "CSR {} inject: {} items={} anti={} ancestry={} stdout={}B top=[{}]",
        hook,
        query_fields(Some(salt), query),
        total_items,
        anti_count,
        ancestry_candidates,
        stdout_bytes,
        top_scores.join(", "),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_contains_only_prompt_length_and_salted_hash() {
        let secret = "secret phrase 🔐\nsecond line";
        let line = detail_line(
            None,
            b"install salt",
            "prompt-submit",
            secret,
            2,
            0,
            300,
            &[],
        )
        .unwrap();
        assert!(!line.contains("secret"));
        assert!(!line.contains("query="));
        assert!(!line.contains('\n'));
        assert!(line.starts_with("CSR prompt-submit inject: q_len=27 q_hash="));
        assert!(line.ends_with(" items=2 anti=0 ancestry=0 stdout=300B top=[]"));
        let hash = line
            .split("q_hash=")
            .nth(1)
            .unwrap()
            .split(' ')
            .next()
            .unwrap();
        assert_eq!(hash.len(), 8);
        assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(
            query_fields(Some(b"install salt"), secret),
            query_fields(Some(b"other"), secret)
        );
    }

    /// The daemon logs the detail line for socket renders, so a client and a
    /// daemon that disagree on the switch must abstain instead of rendering
    /// under the daemon's setting.
    #[test]
    fn timing_detail_switch_is_part_of_the_socket_render_policy() {
        let source = include_str!("prompt_transport.rs");
        let start = source.find("const POLICY_KEYS").unwrap();
        let end = start + source[start..].find("];").unwrap();
        assert!(source[start..end].contains("\"CSR_NO_TIMING_DETAIL\""));
    }

    #[test]
    fn off_switch_skips_detail() {
        assert!(detail_line(Some("1"), b"salt", "prompt-submit", "secret", 0, 0, 0, &[]).is_none());
        assert!(detail_line(Some("0"), b"salt", "prompt-submit", "secret", 0, 0, 0, &[]).is_some());
    }
}
