//! Aggregate parsed log entries into per-hook percentile stats + startup stats.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::parser::Entry;

#[derive(Serialize, Debug, Clone)]
pub struct HookStats {
    pub name: String,
    pub count: usize,
    pub p50_ms: u64,
    pub p95_ms: u64,
    pub p99_ms: u64,
    pub max_ms: u64,
    pub avg_ms: u64,
    pub last_seen: Option<DateTime<Utc>>,
}

#[derive(Serialize, Debug, Clone)]
pub struct StartupStats {
    pub count: usize,
    pub cached_count: usize,
    pub rebuilt_count: usize,
    pub cached_p50_ms: u64,
    pub cached_p95_ms: u64,
    pub rebuilt_p50_ms: u64,
    pub rebuilt_max_ms: u64,
    pub last_chunks: u64,
    pub last_seen: Option<DateTime<Utc>>,
}

#[derive(Serialize, Debug, Clone)]
pub struct TelemetryReport {
    pub hooks: Vec<HookStats>,
    pub startup: StartupStats,
    pub total_hook_invocations: usize,
}

pub fn aggregate(entries: &[Entry]) -> TelemetryReport {
    let mut by_hook: BTreeMap<String, Vec<&Entry>> = BTreeMap::new();
    let mut startups: Vec<&Entry> = Vec::new();

    for e in entries {
        match e {
            Entry::Hook { name, .. } => {
                by_hook.entry(name.clone()).or_default().push(e);
            }
            Entry::Startup { .. } => startups.push(e),
        }
    }

    let mut hooks: Vec<HookStats> = by_hook
        .into_iter()
        .map(|(name, es)| build_hook_stats(name, &es))
        .collect();
    // Sort by p95 descending so slow hooks float to the top.
    hooks.sort_by(|a, b| b.p95_ms.cmp(&a.p95_ms).then_with(|| b.count.cmp(&a.count)));

    let startup = build_startup_stats(&startups);
    let total = hooks.iter().map(|h| h.count).sum();

    TelemetryReport {
        hooks,
        startup,
        total_hook_invocations: total,
    }
}

fn build_hook_stats(name: String, es: &[&Entry]) -> HookStats {
    let mut samples: Vec<u64> = es
        .iter()
        .filter_map(|e| match e {
            Entry::Hook { total_ms, .. } => Some(*total_ms),
            _ => None,
        })
        .collect();
    samples.sort_unstable();
    let count = samples.len();
    let avg = if count > 0 {
        samples.iter().sum::<u64>() / count as u64
    } else {
        0
    };
    let last_seen = es.iter().map(|e| e.ts()).max();

    HookStats {
        name,
        count,
        p50_ms: percentile(&samples, 0.50),
        p95_ms: percentile(&samples, 0.95),
        p99_ms: percentile(&samples, 0.99),
        max_ms: samples.last().copied().unwrap_or(0),
        avg_ms: avg,
        last_seen,
    }
}

fn build_startup_stats(es: &[&Entry]) -> StartupStats {
    let mut cached: Vec<u64> = Vec::new();
    let mut rebuilt: Vec<u64> = Vec::new();
    let mut last_chunks = 0u64;
    let mut last_seen: Option<DateTime<Utc>> = None;

    for e in es {
        if let Entry::Startup {
            ts,
            total_ms,
            rebuilt: r,
            chunks,
        } = e
        {
            if *r {
                rebuilt.push(*total_ms);
            } else {
                cached.push(*total_ms);
            }
            if Some(*ts) > last_seen {
                last_seen = Some(*ts);
                last_chunks = *chunks;
            }
        }
    }
    cached.sort_unstable();
    rebuilt.sort_unstable();

    StartupStats {
        count: cached.len() + rebuilt.len(),
        cached_count: cached.len(),
        rebuilt_count: rebuilt.len(),
        cached_p50_ms: percentile(&cached, 0.50),
        cached_p95_ms: percentile(&cached, 0.95),
        rebuilt_p50_ms: percentile(&rebuilt, 0.50),
        rebuilt_max_ms: rebuilt.last().copied().unwrap_or(0),
        last_chunks,
        last_seen,
    }
}

/// Nearest-rank percentile. Returns 0 for empty input.
fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f64) * p).ceil() as usize;
    let idx = idx.clamp(1, sorted.len()) - 1;
    sorted[idx]
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn hook(name: &str, total_ms: u64) -> Entry {
        Entry::Hook {
            ts: Utc.with_ymd_and_hms(2026, 5, 31, 0, 0, 0).unwrap(),
            name: name.to_string(),
            project: None,
            total_ms,
            hook_ms: total_ms,
        }
    }

    fn startup_at(secs: u32, total_ms: u64, rebuilt: bool, chunks: u64) -> Entry {
        Entry::Startup {
            ts: Utc.with_ymd_and_hms(2026, 5, 31, 0, 0, secs).unwrap(),
            total_ms,
            rebuilt,
            chunks,
        }
    }

    #[test]
    fn percentile_basic() {
        let v: Vec<u64> = (1..=100).collect();
        assert_eq!(percentile(&v, 0.50), 50);
        assert_eq!(percentile(&v, 0.95), 95);
        assert_eq!(percentile(&v, 0.99), 99);
        assert_eq!(percentile(&[], 0.5), 0);
        assert_eq!(percentile(&[42], 0.99), 42);
    }

    #[test]
    fn aggregates_per_hook() {
        let entries: Vec<Entry> = (1..=100)
            .map(|i| hook("stop", i))
            .chain((1..=20).map(|i| hook("post-tool-use", i)))
            .collect();
        let report = aggregate(&entries);
        assert_eq!(report.total_hook_invocations, 120);
        // sorted by p95 desc — "stop" has higher p95 (95) than "post-tool-use" (19)
        assert_eq!(report.hooks[0].name, "stop");
        assert_eq!(report.hooks[0].count, 100);
        assert_eq!(report.hooks[0].p50_ms, 50);
        assert_eq!(report.hooks[0].p95_ms, 95);
        assert_eq!(report.hooks[0].p99_ms, 99);
        assert_eq!(report.hooks[0].max_ms, 100);
        assert_eq!(report.hooks[1].name, "post-tool-use");
    }

    #[test]
    fn aggregates_startup_split() {
        let entries = vec![
            startup_at(1, 80, false, 17000),
            startup_at(2, 90, false, 17100),
            startup_at(3, 100, false, 17200),
            startup_at(4, 13000, true, 17300),
            startup_at(5, 14000, true, 17300),
        ];
        let report = aggregate(&entries);
        assert_eq!(report.startup.count, 5);
        assert_eq!(report.startup.cached_count, 3);
        assert_eq!(report.startup.rebuilt_count, 2);
        assert_eq!(report.startup.cached_p50_ms, 90);
        assert_eq!(report.startup.rebuilt_max_ms, 14000);
        assert_eq!(report.startup.last_chunks, 17300);
    }
}
