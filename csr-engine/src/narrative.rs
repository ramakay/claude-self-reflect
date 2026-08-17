//! Shared logic for AI-narrative `claude -p` invocations (session briefing +
//! session story): opt-out gate, model fallback chain, JSON result parsing,
//! and a persistence-safe content hash.
//!
//! The two call sites keep their own process plumbing (sync vs tokio); only
//! the pure decision/parsing logic lives here.

use serde_json::Value;

/// Kill switch: user disabled all AI-narrative generation.
pub fn narratives_disabled() -> bool {
    std::env::var("CSR_NO_AI_NARRATIVES")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Kill switch: user disabled ratification enrichment.
pub fn ratification_disabled() -> bool {
    std::env::var("CSR_NO_RATIFICATION")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Model candidates in preference order. `None` means omit `--model` and let
/// the claude CLI pick its default — the last-resort path if every Haiku-family
/// alias is decommissioned.
pub fn model_candidates() -> Vec<Option<String>> {
    let mut chain = Vec::with_capacity(3);
    if let Ok(m) = std::env::var("CSR_NARRATIVE_MODEL") {
        let m = m.trim().to_string();
        if !m.is_empty() {
            chain.push(Some(m));
        }
    }
    chain.push(Some("haiku".to_string()));
    chain.push(None);
    chain
}

#[derive(Debug)]
pub struct ParsedNarrative {
    pub text: String,
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
}

/// Parse `claude -p --output-format json` stdout. Returns None on error
/// results, missing text, or unparseable output — callers treat None as
/// "no narrative this time", never as a hook failure.
pub fn parse_claude_json(stdout: &str) -> Option<ParsedNarrative> {
    let v: Value = serde_json::from_str(stdout.trim()).ok()?;
    if v.get("is_error").and_then(Value::as_bool).unwrap_or(false) {
        return None;
    }
    let text = v.get("result")?.as_str()?.trim().to_string();
    if text.is_empty() {
        return None;
    }
    let usage = v.get("usage");
    let get = |key: &str| -> i64 {
        usage
            .and_then(|u| u.get(key))
            .and_then(Value::as_i64)
            .unwrap_or(0)
    };
    let model = v
        .get("modelUsage")
        .and_then(Value::as_object)
        .and_then(|m| m.keys().next().cloned())
        .unwrap_or_else(|| "unknown".to_string());
    Some(ParsedNarrative {
        text,
        model,
        input_tokens: get("input_tokens"),
        output_tokens: get("output_tokens"),
        cache_read_tokens: get("cache_read_input_tokens"),
        cache_creation_tokens: get("cache_creation_input_tokens"),
    })
}

/// Heuristic over stderr (or an error-JSON body): did this invocation fail
/// because the requested model does not exist? Only then do we walk to the
/// next candidate — rate limits and network errors must NOT burn retries
/// across the whole chain.
///
/// Real claude CLI 404 wording (verified 2026-07-11 research probe): exit=1,
/// JSON is_error:true, api_error_status:404, result text "There's an issue
/// with the selected model (X). It may not exist or you may not have access
/// to it. Run --model to pick a different model." — contains NONE of
/// "not found"/"invalid"/"unknown", so a naive substring check on those
/// words alone misses the real-world case. Match the real wording plus
/// defensive legacy variants.
pub fn is_model_not_found(stderr_or_json: &str) -> bool {
    let s = stderr_or_json.to_lowercase();
    (s.contains("issue with the selected model") || s.contains("may not exist"))
        || (s.contains("\"api_error_status\":404") && s.contains("model"))
        || (s.contains("model") && (s.contains("not found") || s.contains("invalid model")))
}

/// Outcome of one `claude -p` attempt. Decides whether the model chain
/// walks to the next candidate (ModelNotFound) or stops (Failed).
#[derive(Debug)]
pub enum AttemptOutcome {
    /// Exit success and stdout parsed as a narrative result.
    Parsed(ParsedNarrative),
    /// The requested model does not exist / is inaccessible — walk the chain.
    ModelNotFound,
    /// Any other failure (rate limit, network, garbage output) — stop; never
    /// burn remaining candidates on non-model errors.
    Failed(String),
}

pub fn classify_attempt(status_success: bool, stdout: &str, stderr: &str) -> AttemptOutcome {
    if status_success {
        match parse_claude_json(stdout) {
            Some(p) => AttemptOutcome::Parsed(p),
            None if is_model_not_found(stdout) => AttemptOutcome::ModelNotFound,
            None => AttemptOutcome::Failed("claude -p returned unparseable/error JSON".to_string()),
        }
    } else if is_model_not_found(stderr) || is_model_not_found(stdout) {
        AttemptOutcome::ModelNotFound
    } else {
        AttemptOutcome::Failed(format!(
            "claude -p failed: {}",
            if stderr.is_empty() { stdout } else { stderr }
        ))
    }
}

/// FNV-1a 64-bit. Deterministic across processes and versions (unlike
/// std's DefaultHasher), so the digest can be persisted in the meta table.
pub fn fnv1a_64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/fixtures/claude_p_result.json");

    #[test]
    fn test_parse_real_fixture() {
        let p = parse_claude_json(FIXTURE).expect("fixture must parse");
        assert!(!p.text.is_empty());
        assert!(p.input_tokens > 0);
        assert!(p.output_tokens > 0);
        assert_ne!(p.model, ""); // "unknown" acceptable, empty is not
    }

    #[test]
    fn test_parse_rejects_error_result() {
        let json = r#"{"is_error": true, "result": "boom", "usage": {"input_tokens": 1, "output_tokens": 1}}"#;
        assert!(parse_claude_json(json).is_none());
    }

    #[test]
    fn test_parse_rejects_garbage() {
        assert!(parse_claude_json("not json").is_none());
        assert!(parse_claude_json("{}").is_none());
    }

    #[test]
    fn test_model_candidates() {
        // Merged (no serial_test dep available): sequential assertions avoid
        // races on the process-global CSR_NARRATIVE_MODEL env var.
        std::env::remove_var("CSR_NARRATIVE_MODEL");
        let c = model_candidates();
        assert_eq!(c, vec![Some("haiku".to_string()), None]);

        std::env::set_var("CSR_NARRATIVE_MODEL", "sonnet");
        let c = model_candidates();
        assert_eq!(c[0], Some("sonnet".to_string()));
        assert_eq!(c[1], Some("haiku".to_string()));
        assert_eq!(c[2], None);
        std::env::remove_var("CSR_NARRATIVE_MODEL");
    }

    #[test]
    fn test_is_model_not_found() {
        // Verbatim real CLI 404 wording (Task 1 research probe)
        assert!(is_model_not_found(
            "There's an issue with the selected model (no-such-model-zz9). It may not exist or you may not have access to it."
        ));
        assert!(is_model_not_found(
            r#"{"is_error":true,"api_error_status":404,"result":"There's an issue with the selected model"}"#
        ));
        // Defensive legacy variants
        assert!(is_model_not_found("Error: model 'zz9' not found"));
        assert!(is_model_not_found("invalid model specified"));
        // Must NOT trip on unrelated failures
        assert!(!is_model_not_found("rate limit exceeded"));
        assert!(!is_model_not_found("network error"));
        assert!(!is_model_not_found("Invalid API key"));
    }

    #[test]
    fn test_fnv1a_stable_across_runs() {
        // FNV-1a is deterministic — unlike DefaultHasher (SipHash, random per-process
        // seed), which is why we can persist it in the meta table.
        assert_eq!(fnv1a_64(b"hello"), 0xa430d84680aabd0b);
        assert_ne!(fnv1a_64(b"hello"), fnv1a_64(b"hello "));
    }

    #[test]
    fn test_narratives_disabled_env() {
        // `CSR_NO_AI_NARRATIVES` is also read by `dream::strategy::category_disabled`,
        // whose own tests mutate it under this same shared crate-level guard —
        // without it, `cargo test`'s cross-module thread parallelism can stomp
        // one test's `set_var`/`remove_var` with another's mid-assertion.
        let _g = crate::daemon::dream_cadence::env_test_guard();
        std::env::remove_var("CSR_NO_AI_NARRATIVES");
        assert!(!narratives_disabled());
        std::env::set_var("CSR_NO_AI_NARRATIVES", "1");
        assert!(narratives_disabled());
        std::env::set_var("CSR_NO_AI_NARRATIVES", "true");
        assert!(narratives_disabled());
        std::env::set_var("CSR_NO_AI_NARRATIVES", "0");
        assert!(!narratives_disabled());
        std::env::remove_var("CSR_NO_AI_NARRATIVES");
    }

    #[test]
    fn test_ratification_disabled_env() {
        std::env::remove_var("CSR_NO_RATIFICATION");
        assert!(!ratification_disabled());
        std::env::set_var("CSR_NO_RATIFICATION", "1");
        assert!(ratification_disabled());
        std::env::set_var("CSR_NO_RATIFICATION", "true");
        assert!(ratification_disabled());
        std::env::set_var("CSR_NO_RATIFICATION", "0");
        assert!(!ratification_disabled());
        std::env::remove_var("CSR_NO_RATIFICATION");
    }

    #[test]
    fn test_classify_attempt_success_fixture() {
        match classify_attempt(true, FIXTURE, "") {
            AttemptOutcome::Parsed(p) => assert!(!p.text.is_empty()),
            other => panic!("expected Parsed, got {:?}", other),
        }
    }

    #[test]
    fn test_classify_attempt_model_404_json_on_failure() {
        let json = r#"{"is_error":true,"api_error_status":404,"result":"There's an issue with the selected model (zz9). It may not exist or you may not have access to it."}"#;
        assert!(matches!(
            classify_attempt(false, json, ""),
            AttemptOutcome::ModelNotFound
        ));
        assert!(matches!(
            classify_attempt(false, "", json),
            AttemptOutcome::ModelNotFound
        ));
    }

    #[test]
    fn test_classify_attempt_model_404_wording_on_success_status() {
        let json = r#"{"is_error":true,"result":"There's an issue with the selected model (zz9). It may not exist."}"#;
        assert!(matches!(
            classify_attempt(true, json, ""),
            AttemptOutcome::ModelNotFound
        ));
    }

    #[test]
    fn test_classify_attempt_rate_limit_is_failed() {
        assert!(matches!(
            classify_attempt(false, "", "rate limit exceeded"),
            AttemptOutcome::Failed(_)
        ));
    }

    #[test]
    fn test_classify_attempt_network_error_is_failed() {
        assert!(matches!(
            classify_attempt(false, "network error: connection refused", ""),
            AttemptOutcome::Failed(_)
        ));
    }

    #[test]
    fn test_classify_attempt_garbage_on_success_is_failed() {
        assert!(matches!(
            classify_attempt(true, "not json at all", ""),
            AttemptOutcome::Failed(_)
        ));
    }

    #[test]
    fn test_classify_attempt_failed_prefers_stderr() {
        match classify_attempt(false, "stdout noise", "real error") {
            AttemptOutcome::Failed(msg) => assert!(msg.contains("real error")),
            other => panic!("expected Failed, got {:?}", other),
        }
    }
}
