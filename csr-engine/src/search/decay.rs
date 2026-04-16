use chrono::{DateTime, Utc};

/// Default decay weight (0.3 means 30% of score is time-dependent).
const DEFAULT_DECAY_WEIGHT: f64 = 0.3;

/// Default half-life in days (score halves after this many days).
const DEFAULT_SCALE_DAYS: f64 = 90.0;

/// Apply time-based decay to a search score.
///
/// Formula (identical to Python `decay_manager.py`):
///   adjusted = score * ((1 - DECAY_WEIGHT) + DECAY_WEIGHT * 2^(-age_days / SCALE_DAYS))
///
/// A 90-day-old result with score=1.0:
///   1.0 * (0.7 + 0.3 * 2^(-90/90)) = 1.0 * (0.7 + 0.3 * 0.5) = 0.85
pub fn apply_decay(
    score: f32,
    timestamp: &DateTime<Utc>,
    now: &DateTime<Utc>,
    decay_weight: Option<f64>,
    scale_days: Option<f64>,
) -> f32 {
    let weight = decay_weight.unwrap_or(DEFAULT_DECAY_WEIGHT);
    let scale = scale_days.unwrap_or(DEFAULT_SCALE_DAYS);

    let age_days = (*now - *timestamp).num_seconds() as f64 / 86400.0;
    if age_days <= 0.0 {
        return score;
    }

    let time_factor = 2.0_f64.powf(-age_days / scale);
    let adjusted = (score as f64) * ((1.0 - weight) + weight * time_factor);
    adjusted as f32
}

/// Configurable decay parameters for different contexts.
/// Injection needs faster decay (30-day half-life) to prioritize recent context.
/// Search uses slower decay (90-day half-life) to keep older results findable.
#[derive(Debug, Clone)]
pub struct DecayConfig {
    pub decay_weight: f64,
    pub base_half_life_days: f64,
}

impl Default for DecayConfig {
    fn default() -> Self {
        Self {
            decay_weight: DEFAULT_DECAY_WEIGHT,
            base_half_life_days: DEFAULT_SCALE_DAYS,
        }
    }
}

impl DecayConfig {
    /// Injection context: faster decay, recent context matters more.
    pub fn for_injection() -> Self {
        Self {
            decay_weight: 0.5,
            base_half_life_days: 30.0,
        }
    }

    /// Search context: slower decay, older results still valuable.
    pub fn for_search() -> Self {
        Self {
            decay_weight: DEFAULT_DECAY_WEIGHT,
            base_half_life_days: DEFAULT_SCALE_DAYS,
        }
    }
}

/// Unified decay function using DecayConfig.
/// Replaces both `apply_decay` (for search) and `compute_recency_boost` (for injection)
/// to eliminate the recency double-counting bug.
pub fn apply_decay_unified(
    score: f32,
    timestamp: &DateTime<Utc>,
    now: &DateTime<Utc>,
    config: &DecayConfig,
) -> f32 {
    let age_days = (*now - *timestamp).num_seconds() as f64 / 86400.0;
    if age_days <= 0.0 {
        return score;
    }
    let time_factor = 2.0_f64.powf(-age_days / config.base_half_life_days);
    let adjusted =
        (score as f64) * ((1.0 - config.decay_weight) + config.decay_weight * time_factor);
    adjusted as f32
}

/// A retrieval event for TAD (Temporal Attention Decay).
/// Tracks when a memory was surfaced and whether the session succeeded.
#[derive(Debug, Clone)]
pub struct RetrievalEvent {
    pub retrieved_at: DateTime<Utc>,
    pub session_outcome: SessionOutcome,
}

/// Outcome of the session where a memory was retrieved.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SessionOutcome {
    Success,
    Failed,
    Neutral,
}

/// Apply Temporal Attention Decay: memories that helped in successful sessions
/// persist longer (half-life increases), while memories from failed sessions
/// decay faster (half-life decreases).
///
/// With no retrieval events, behaves identically to `apply_decay_unified`.
pub fn apply_tad(
    score: f32,
    timestamp: &DateTime<Utc>,
    now: &DateTime<Utc>,
    retrieval_events: &[RetrievalEvent],
    config: &DecayConfig,
) -> f32 {
    let reinforcement = compute_reinforcement(retrieval_events, now);
    let effective_half_life = config.base_half_life_days * 2.0_f64.powf(reinforcement);

    let age_days = (*now - *timestamp).num_seconds() as f64 / 86400.0;
    if age_days <= 0.0 {
        return score;
    }

    let time_factor = 2.0_f64.powf(-age_days / effective_half_life);
    let adjusted =
        (score as f64) * ((1.0 - config.decay_weight) + config.decay_weight * time_factor);
    adjusted as f32
}

/// Compute reinforcement score from retrieval events.
/// Recent successful retrievals increase the score (slower decay).
/// Recent failed retrievals decrease it (faster decay).
/// Clamped to [-2.0, 2.0] to prevent extreme half-life swings.
fn compute_reinforcement(events: &[RetrievalEvent], now: &DateTime<Utc>) -> f64 {
    if events.is_empty() {
        return 0.0;
    }

    let mut score = 0.0;
    for event in events {
        let days_ago = (*now - event.retrieved_at).num_seconds() as f64 / 86400.0;
        let recency_weight = 2.0_f64.powf(-days_ago / 30.0);
        let outcome_weight = match event.session_outcome {
            SessionOutcome::Success => 1.0,
            SessionOutcome::Neutral => 0.0,
            SessionOutcome::Failed => -1.0,
        };
        score += outcome_weight * recency_weight;
    }

    score.clamp(-2.0, 2.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn test_decay_90_days() {
        let now = Utc::now();
        let past = now - Duration::days(90);
        let result = apply_decay(1.0, &past, &now, None, None);
        // Expected: 1.0 * (0.7 + 0.3 * 0.5) = 0.85
        assert!((result - 0.85).abs() < 0.01, "got {result}");
    }

    #[test]
    fn test_decay_zero_age() {
        let now = Utc::now();
        let result = apply_decay(0.9, &now, &now, None, None);
        assert!((result - 0.9).abs() < 0.001);
    }

    #[test]
    fn test_decay_very_old() {
        let now = Utc::now();
        let past = now - Duration::days(365);
        let result = apply_decay(1.0, &past, &now, None, None);
        // Should be significantly decayed but still > 0.7 (the base)
        assert!(result > 0.7);
        assert!(result < 0.75);
    }

    // --- DecayConfig tests ---

    #[test]
    fn test_decay_config_for_injection() {
        let config = DecayConfig::for_injection();
        assert_eq!(config.base_half_life_days, 30.0);
        assert_eq!(config.decay_weight, 0.5);
    }

    #[test]
    fn test_decay_config_for_search() {
        let config = DecayConfig::for_search();
        assert_eq!(config.base_half_life_days, 90.0);
        assert_eq!(config.decay_weight, 0.3);
    }

    #[test]
    fn test_unified_decay_matches_original() {
        let now = Utc::now();
        let past = now - Duration::days(90);
        let config = DecayConfig::for_search();
        let unified = apply_decay_unified(1.0, &past, &now, &config);
        let original = apply_decay(1.0, &past, &now, None, None);
        assert!(
            (unified - original).abs() < 0.001,
            "unified={} original={}",
            unified,
            original
        );
    }

    // --- TAD tests ---

    #[test]
    fn test_tad_reinforced_memory_decays_slower() {
        let now = Utc::now();
        let past = now - Duration::days(90);
        let config = DecayConfig::for_search();

        let events = vec![RetrievalEvent {
            retrieved_at: now - Duration::days(10),
            session_outcome: SessionOutcome::Success,
        }];

        let standard = apply_decay_unified(1.0, &past, &now, &config);
        let reinforced = apply_tad(1.0, &past, &now, &events, &config);
        assert!(
            reinforced > standard,
            "reinforced={} should be > standard={}",
            reinforced,
            standard
        );
    }

    #[test]
    fn test_tad_failed_memory_decays_faster() {
        let now = Utc::now();
        let past = now - Duration::days(90);
        let config = DecayConfig::for_search();

        let events = vec![RetrievalEvent {
            retrieved_at: now - Duration::days(5),
            session_outcome: SessionOutcome::Failed,
        }];

        let standard = apply_decay_unified(1.0, &past, &now, &config);
        let suppressed = apply_tad(1.0, &past, &now, &events, &config);
        assert!(
            suppressed < standard,
            "suppressed={} should be < standard={}",
            suppressed,
            standard
        );
    }

    #[test]
    fn test_tad_no_events_equals_standard() {
        let now = Utc::now();
        let past = now - Duration::days(90);
        let config = DecayConfig::for_search();
        let standard = apply_decay_unified(1.0, &past, &now, &config);
        let tad = apply_tad(1.0, &past, &now, &[], &config);
        assert!((tad - standard).abs() < 0.001);
    }

    #[test]
    fn test_reinforcement_clamping() {
        let now = Utc::now();
        // Many recent successes should clamp to 2.0
        let events: Vec<RetrievalEvent> = (0..10)
            .map(|i| RetrievalEvent {
                retrieved_at: now - Duration::days(i),
                session_outcome: SessionOutcome::Success,
            })
            .collect();
        let r = compute_reinforcement(&events, &now);
        assert!(r <= 2.0, "reinforcement={} should be clamped to 2.0", r);
    }
}
