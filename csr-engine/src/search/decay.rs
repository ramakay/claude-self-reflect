use chrono::{DateTime, Utc};

/// Default decay weight (0.3 means 30% of score is time-dependent).
const DEFAULT_DECAY_WEIGHT: f64 = 0.3;

/// Default half-life in days (score halves after this many days).
const DEFAULT_SCALE_DAYS: f64 = 90.0;

/// Each shipped release behind increases effective age by 25%.
pub const ANCESTRY_RELEASE_STEP: f64 = 0.25;

/// Release ancestry never lowers a surface's effective half-life below 25%.
pub const ANCESTRY_MIN_HALF_LIFE_RATIO: f64 = 0.25;

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
    apply_decay_with_age_multiplier(score, timestamp, now, decay_weight, scale_days, 1.0)
}

/// Apply wall-clock decay plus deterministic release ancestry. The neutral
/// cases call [`apply_decay`] directly to preserve pre-change floating-point
/// behavior exactly.
pub fn apply_decay_with_release_ancestry(
    score: f32,
    timestamp: &DateTime<Utc>,
    now: &DateTime<Utc>,
    decay_weight: Option<f64>,
    scale_days: Option<f64>,
    releases_behind: Option<u32>,
) -> f32 {
    let multiplier = ancestry_age_multiplier(releases_behind);
    if multiplier == 1.0 {
        apply_decay(score, timestamp, now, decay_weight, scale_days)
    } else {
        apply_decay_with_age_multiplier(score, timestamp, now, decay_weight, scale_days, multiplier)
    }
}

/// Apply time-based decay after multiplying the memory's effective age.
///
/// `age_multiplier` must be finite and at least `1.0`. Invalid values trip a
/// debug assertion and fall back to `1.0` in release builds. A multiplier of
/// `1.0` is identical to [`apply_decay`].
pub fn apply_decay_with_age_multiplier(
    score: f32,
    timestamp: &DateTime<Utc>,
    now: &DateTime<Utc>,
    decay_weight: Option<f64>,
    scale_days: Option<f64>,
    age_multiplier: f64,
) -> f32 {
    let age_multiplier = validate_age_multiplier(age_multiplier);
    let weight = decay_weight.unwrap_or(DEFAULT_DECAY_WEIGHT);
    let scale = scale_days.unwrap_or(DEFAULT_SCALE_DAYS);

    let age_days = (*now - *timestamp).num_seconds() as f64 / 86400.0 * age_multiplier;
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
/// With no retrieval events, behaves identically to `apply_decay` with default config.
pub fn apply_tad(
    score: f32,
    timestamp: &DateTime<Utc>,
    now: &DateTime<Utc>,
    retrieval_events: &[RetrievalEvent],
    config: &DecayConfig,
) -> f32 {
    apply_tad_with_age_multiplier(score, timestamp, now, retrieval_events, config, 1.0)
}

/// Apply TAD plus deterministic release-ancestry decay. Missing labels,
/// unreleased work (represented by `None`), and the newest release preserve
/// the pre-change TAD path bit-for-bit.
pub fn apply_tad_with_release_ancestry(
    score: f32,
    timestamp: &DateTime<Utc>,
    now: &DateTime<Utc>,
    retrieval_events: &[RetrievalEvent],
    config: &DecayConfig,
    releases_behind: Option<u32>,
) -> f32 {
    let multiplier = ancestry_age_multiplier(releases_behind);
    if multiplier == 1.0 {
        apply_tad(score, timestamp, now, retrieval_events, config)
    } else {
        apply_tad_with_age_multiplier(score, timestamp, now, retrieval_events, config, multiplier)
    }
}

/// Equivalent to `1 / half_life_ratio`, capped so the effective half-life
/// never drops below [`ANCESTRY_MIN_HALF_LIFE_RATIO`] of the surface default.
pub fn ancestry_age_multiplier(releases_behind: Option<u32>) -> f64 {
    let Some(releases_behind) = releases_behind.filter(|n| *n > 0) else {
        return 1.0;
    };
    (1.0 + ANCESTRY_RELEASE_STEP * f64::from(releases_behind))
        .min(1.0 / ANCESTRY_MIN_HALF_LIFE_RATIO)
}

/// Apply TAD after multiplying the memory's effective age. Reinforcement
/// still changes the half-life exactly as before; only elapsed age is scaled.
///
/// `age_multiplier` must be finite and at least `1.0`. Invalid values trip a
/// debug assertion and fall back to `1.0` in release builds. A multiplier of
/// `1.0` is identical to [`apply_tad`].
pub fn apply_tad_with_age_multiplier(
    score: f32,
    timestamp: &DateTime<Utc>,
    now: &DateTime<Utc>,
    retrieval_events: &[RetrievalEvent],
    config: &DecayConfig,
    age_multiplier: f64,
) -> f32 {
    let age_multiplier = validate_age_multiplier(age_multiplier);
    let reinforcement = compute_reinforcement(retrieval_events, now);
    let effective_half_life = config.base_half_life_days * 2.0_f64.powf(reinforcement);

    let age_days = (*now - *timestamp).num_seconds() as f64 / 86400.0 * age_multiplier;
    if age_days <= 0.0 {
        return score;
    }

    let time_factor = 2.0_f64.powf(-age_days / effective_half_life);
    let adjusted =
        (score as f64) * ((1.0 - config.decay_weight) + config.decay_weight * time_factor);
    adjusted as f32
}

fn validate_age_multiplier(age_multiplier: f64) -> f64 {
    let valid = age_multiplier.is_finite() && age_multiplier >= 1.0;
    debug_assert!(
        valid,
        "age_multiplier must be finite and at least 1.0, got {age_multiplier:?}"
    );
    if valid {
        age_multiplier
    } else {
        1.0
    }
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
    fn test_tad_no_events_matches_original() {
        let now = Utc::now();
        let past = now - Duration::days(90);
        let config = DecayConfig::for_search();
        let tad = apply_tad(1.0, &past, &now, &[], &config);
        let original = apply_decay(1.0, &past, &now, None, None);
        assert!(
            (tad - original).abs() < 0.001,
            "tad={} original={}",
            tad,
            original
        );
    }

    #[test]
    fn age_multiplier_one_is_bit_identical_to_existing_tad() {
        let now = Utc::now();
        let past = now - Duration::days(90);
        let config = DecayConfig::for_search();

        let current = apply_tad(0.91, &past, &now, &[], &config);
        let multiplied = apply_tad_with_age_multiplier(0.91, &past, &now, &[], &config, 1.0);

        assert_eq!(multiplied.to_bits(), current.to_bits());
    }

    #[test]
    fn larger_age_multiplier_decays_faster() {
        let now = Utc::now();
        let past = now - Duration::days(90);
        let config = DecayConfig::for_search();

        let standard = apply_tad_with_age_multiplier(1.0, &past, &now, &[], &config, 1.0);
        let accelerated = apply_tad_with_age_multiplier(1.0, &past, &now, &[], &config, 3.0);

        assert!(
            accelerated < standard,
            "accelerated={accelerated} should be < standard={standard}"
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    fn invalid_age_multipliers_trigger_debug_assertions() {
        let now = Utc::now();
        let past = now - Duration::days(90);

        for multiplier in [f64::NAN, f64::INFINITY, 0.0, -1.0] {
            let decay_panicked = std::panic::catch_unwind(|| {
                apply_decay_with_age_multiplier(1.0, &past, &now, None, None, multiplier)
            });
            assert!(
                decay_panicked.is_err(),
                "invalid multiplier {multiplier:?} must trip the debug assertion"
            );

            let tad_panicked = std::panic::catch_unwind(|| {
                apply_tad_with_age_multiplier(
                    1.0,
                    &past,
                    &now,
                    &[],
                    &DecayConfig::for_search(),
                    multiplier,
                )
            });
            assert!(
                tad_panicked.is_err(),
                "invalid multiplier {multiplier:?} must trip the TAD debug assertion"
            );
        }
    }

    #[cfg(not(debug_assertions))]
    #[test]
    fn invalid_age_multipliers_fall_back_to_one_in_release() {
        let now = Utc::now();
        let past = now - Duration::days(90);
        let config = DecayConfig::for_search();
        let expected_decay = apply_decay(1.0, &past, &now, None, None);
        let expected_tad = apply_tad(1.0, &past, &now, &[], &config);

        for multiplier in [f64::NAN, f64::INFINITY, 0.0, -1.0] {
            assert_eq!(
                apply_decay_with_age_multiplier(1.0, &past, &now, None, None, multiplier,)
                    .to_bits(),
                expected_decay.to_bits(),
                "invalid multiplier {multiplier:?} must behave like 1.0"
            );
            assert_eq!(
                apply_tad_with_age_multiplier(1.0, &past, &now, &[], &config, multiplier).to_bits(),
                expected_tad.to_bits(),
                "invalid TAD multiplier {multiplier:?} must behave like 1.0"
            );
        }
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

        let standard = apply_tad(1.0, &past, &now, &[], &config);
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

        let standard = apply_tad(1.0, &past, &now, &[], &config);
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
        let standard = apply_decay(1.0, &past, &now, None, None);
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
