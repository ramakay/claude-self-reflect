//! Lifecycle-aware weight profiles for LAPI (Lifecycle-Aware Predictive Injection).
//!
//! Different hook phases need different retrieval priorities:
//! - SessionStart: strategies + anti-patterns (big picture)
//! - PromptSubmit: code context + error solutions (specific help)
//! - Stop: stuck patterns + iteration learnings (escape hatch)
//! - PreCompact: session state (preserve work)

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HookPhase {
    SessionStart,
    PromptSubmit,
    Stop,
    PreCompact,
}

#[derive(Debug, Clone, Copy)]
pub struct WeightProfile {
    pub semantic: f32,
    pub recency: f32,
    pub file_overlap: f32,
    pub error_match: f32,
    pub phase_boost: f32,
}

impl WeightProfile {
    pub fn for_phase(phase: HookPhase) -> Self {
        match phase {
            HookPhase::SessionStart => Self {
                semantic: 0.25,
                recency: 0.10,
                file_overlap: 0.15,
                error_match: 0.10,
                phase_boost: 0.40,
            },
            HookPhase::PromptSubmit => Self {
                semantic: 0.40,
                recency: 0.15,
                file_overlap: 0.20,
                error_match: 0.10,
                phase_boost: 0.15,
            },
            HookPhase::Stop => Self {
                semantic: 0.20,
                recency: 0.10,
                file_overlap: 0.10,
                error_match: 0.25,
                phase_boost: 0.35,
            },
            HookPhase::PreCompact => Self {
                semantic: 0.30,
                recency: 0.30,
                file_overlap: 0.15,
                error_match: 0.05,
                phase_boost: 0.20,
            },
        }
    }
}

/// Phase-specific boost: how well does this result's TYPE match what this phase needs?
pub fn compute_phase_boost(source: &str, tags: &[String], phase: HookPhase) -> f32 {
    match phase {
        HookPhase::SessionStart => {
            if tags.iter().any(|t| t.starts_with("outcome_")) {
                return 1.0;
            }
            if source == "anti_pattern" {
                return 0.9;
            }
            if source == "reflection" {
                return 0.7;
            }
            0.2
        }
        HookPhase::PromptSubmit => {
            if source == "chunk" {
                return 0.8;
            }
            if tags.iter().any(|t| t == "error_recovery") {
                return 1.0;
            }
            if source == "reflection" {
                return 0.5;
            }
            0.3
        }
        HookPhase::Stop => {
            if source == "anti_pattern" {
                return 1.0;
            }
            if tags.iter().any(|t| t.starts_with("iteration_")) {
                return 0.9;
            }
            0.2
        }
        HookPhase::PreCompact => {
            if tags.iter().any(|t| t == "ralph_session") {
                return 1.0;
            }
            if source == "reflection" {
                return 0.7;
            }
            0.3
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_weight_profiles_sum_to_one() {
        for phase in [
            HookPhase::SessionStart,
            HookPhase::PromptSubmit,
            HookPhase::Stop,
            HookPhase::PreCompact,
        ] {
            let w = WeightProfile::for_phase(phase);
            let sum = w.semantic + w.recency + w.file_overlap + w.error_match + w.phase_boost;
            assert!(
                (sum - 1.0).abs() < 0.001,
                "Phase {:?} weights sum to {}, not 1.0",
                phase,
                sum
            );
        }
    }

    #[test]
    fn test_session_start_prefers_phase_boost() {
        let w = WeightProfile::for_phase(HookPhase::SessionStart);
        assert!(
            w.phase_boost > w.semantic,
            "SessionStart should boost phase-appropriate results most"
        );
    }

    #[test]
    fn test_prompt_submit_prefers_semantic() {
        let w = WeightProfile::for_phase(HookPhase::PromptSubmit);
        assert!(
            w.semantic >= w.phase_boost,
            "PromptSubmit should weight semantic match highest"
        );
    }

    #[test]
    fn test_stop_prefers_error_match() {
        let w = WeightProfile::for_phase(HookPhase::Stop);
        assert!(
            w.error_match + w.phase_boost > w.semantic,
            "Stop should weight stuck-detection signals highly"
        );
    }

    #[test]
    fn test_phase_boost_computation() {
        // Anti-pattern should score high at SessionStart
        let score = compute_phase_boost("anti_pattern", &[], HookPhase::SessionStart);
        assert!(score > 0.8);
        // Chunk should score high at PromptSubmit
        let score = compute_phase_boost("chunk", &[], HookPhase::PromptSubmit);
        assert!(score > 0.7);
        // Anti-pattern should score high at Stop
        let score = compute_phase_boost("anti_pattern", &[], HookPhase::Stop);
        assert!(score > 0.9);
    }
}
