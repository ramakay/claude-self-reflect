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
}
