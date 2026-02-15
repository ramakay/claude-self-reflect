//! Stuck loop detection for Ralph sessions.
//!
//! Detects when a session is stuck in a loop by analyzing:
//! 1. Error repetition (same normalized error ≥3 times)
//! 2. High iteration count with low confidence (iteration ≥20, confidence <30)
//! 3. Failed approach accumulation (≥5 failed approaches)
//! 4. Declining confidence (not tracked in current RalphState — future enhancement)

use crate::hooks::ralph_state::RalphState;

/// Result of stuck-loop analysis.
#[derive(Debug)]
pub struct StuckAnalysis {
    pub is_stuck: bool,
    pub reasons: Vec<String>,
    pub severity: StuckSeverity,
}

/// How severe the stuck condition is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StuckSeverity {
    /// No issues detected
    Normal,
    /// One signal triggered — likely a minor issue
    Warning,
    /// Multiple signals or severe signal — intervention needed
    Critical,
}

/// Analyze Ralph state for stuck-loop indicators.
pub fn analyze(ralph: &RalphState) -> StuckAnalysis {
    let mut reasons = Vec::new();

    // Signal 1: Error repetition — same error ≥3 times
    for (sig, count) in &ralph.error_signatures {
        if *count >= 3 {
            reasons.push(format!(
                "Error repeated {}x: {}",
                count,
                truncate(sig, 80)
            ));
        }
    }

    // Signal 2: High iteration count + low confidence
    if ralph.iteration >= 20 && ralph.exit_confidence < 30 {
        reasons.push(format!(
            "High iteration ({}) with low confidence ({}%)",
            ralph.iteration, ralph.exit_confidence
        ));
    }

    // Signal 3: Failed approach accumulation
    if ralph.failed_approaches.len() >= 5 {
        reasons.push(format!(
            "{} failed approaches accumulated",
            ralph.failed_approaches.len()
        ));
    }

    let severity = match reasons.len() {
        0 => StuckSeverity::Normal,
        1 => StuckSeverity::Warning,
        _ => StuckSeverity::Critical,
    };

    StuckAnalysis {
        is_stuck: !reasons.is_empty(),
        reasons,
        severity,
    }
}

/// Format stuck warning for injection. Returns None if not stuck.
pub fn format_warning(analysis: &StuckAnalysis) -> Option<String> {
    if !analysis.is_stuck {
        return None;
    }

    let severity_label = match analysis.severity {
        StuckSeverity::Normal => return None,
        StuckSeverity::Warning => "WARNING",
        StuckSeverity::Critical => "CRITICAL",
    };

    let mut warning = format!("[{}] Session may be stuck:\n", severity_label);
    for reason in &analysis.reasons {
        warning.push_str(&format!("- {}\n", reason));
    }
    warning.push_str("Consider: changing approach, simplifying scope, or requesting user guidance.");

    Some(warning)
}

/// Truncate string at safe char boundary (H-4 fix).
fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..s.floor_char_boundary(max)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_ralph(
        iteration: usize,
        exit_confidence: u8,
        error_sigs: Vec<(&str, usize)>,
        failed: Vec<&str>,
    ) -> RalphState {
        RalphState {
            iteration,
            exit_confidence,
            error_signatures: error_sigs
                .into_iter()
                .map(|(s, c)| (s.to_string(), c))
                .collect(),
            failed_approaches: failed.into_iter().map(|s| s.to_string()).collect(),
            active: true,
            ..Default::default()
        }
    }

    #[test]
    fn test_normal_state() {
        let ralph = make_ralph(5, 80, vec![("error", 1)], vec!["one"]);
        let result = analyze(&ralph);
        assert!(!result.is_stuck);
        assert_eq!(result.severity, StuckSeverity::Normal);
    }

    #[test]
    fn test_error_repetition() {
        let ralph = make_ralph(5, 80, vec![("JWT expired", 3)], vec![]);
        let result = analyze(&ralph);
        assert!(result.is_stuck);
        assert_eq!(result.severity, StuckSeverity::Warning);
        assert!(result.reasons[0].contains("repeated 3x"));
    }

    #[test]
    fn test_high_iteration_low_confidence() {
        let ralph = make_ralph(25, 20, vec![], vec![]);
        let result = analyze(&ralph);
        assert!(result.is_stuck);
        assert!(result.reasons[0].contains("High iteration"));
    }

    #[test]
    fn test_many_failed_approaches() {
        let ralph = make_ralph(
            5,
            50,
            vec![],
            vec!["a", "b", "c", "d", "e"],
        );
        let result = analyze(&ralph);
        assert!(result.is_stuck);
        assert!(result.reasons[0].contains("5 failed approaches"));
    }

    #[test]
    fn test_critical_multiple_signals() {
        let ralph = make_ralph(
            25,
            10,
            vec![("timeout", 5)],
            vec!["a", "b", "c", "d", "e"],
        );
        let result = analyze(&ralph);
        assert!(result.is_stuck);
        assert_eq!(result.severity, StuckSeverity::Critical);
        assert!(result.reasons.len() >= 2);
    }

    #[test]
    fn test_format_warning_normal() {
        let analysis = StuckAnalysis {
            is_stuck: false,
            reasons: vec![],
            severity: StuckSeverity::Normal,
        };
        assert!(format_warning(&analysis).is_none());
    }

    #[test]
    fn test_format_warning_critical() {
        let analysis = StuckAnalysis {
            is_stuck: true,
            reasons: vec!["Error repeated 5x".into(), "High iteration (30)".into()],
            severity: StuckSeverity::Critical,
        };
        let warning = format_warning(&analysis).unwrap();
        assert!(warning.contains("[CRITICAL]"));
        assert!(warning.contains("Error repeated 5x"));
    }
}
