//! Opt-in per-call stage timer for the MCP reflect path.
//!
//! With `CSR_STAGE_TIMING_FILE` set, each `csr_reflect_on_past` call appends
//! one NDJSON line to that file: the query hash, the total, the number of
//! gather passes and the milliseconds spent in each stage. The variable is
//! read once. With it unset every entry point returns at once and nothing is
//! written anywhere (never stdout, which is the JSON-RPC channel, and never
//! stderr).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::future::Future;
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Instant;

fn target() -> &'static Option<PathBuf> {
    static TARGET: OnceLock<Option<PathBuf>> = OnceLock::new();
    TARGET.get_or_init(|| {
        std::env::var_os("CSR_STAGE_TIMING_FILE")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    })
}

/// True when `CSR_STAGE_TIMING_FILE` named a file at first use.
pub(crate) fn enabled() -> bool {
    target().is_some()
}

/// The stages and counters of one call.
pub(crate) struct CallTimer {
    query_hash: String,
    started: Instant,
    stages: BTreeMap<&'static str, f64>,
    counts: BTreeMap<&'static str, u64>,
}

impl CallTimer {
    fn new(query: &str) -> Self {
        let hash = blake3::hash(query.as_bytes()).to_hex();
        Self {
            query_hash: hash.as_str()[..16].to_string(),
            started: Instant::now(),
            stages: BTreeMap::new(),
            counts: BTreeMap::new(),
        }
    }

    /// Add the time since `started` to stage `name` (a stage that runs in both
    /// passes accumulates).
    pub(crate) fn stage(&mut self, name: &'static str, started: Instant) {
        *self.stages.entry(name).or_insert(0.0) += started.elapsed().as_secs_f64() * 1000.0;
    }

    /// Add `n` to counter `name`.
    pub(crate) fn count(&mut self, name: &'static str, n: u64) {
        *self.counts.entry(name).or_insert(0) += n;
    }

    fn max_count(&mut self, name: &'static str, n: u64) {
        let slot = self.counts.entry(name).or_insert(0);
        *slot = (*slot).max(n);
    }

    /// Append this call's NDJSON line. A no-op unless the file is set; a write
    /// failure is dropped, the search never fails on its timer.
    pub(crate) fn finish(self) {
        let Some(path) = target().as_ref() else {
            return;
        };
        let line = serde_json::json!({
            "query_hash": self.query_hash,
            "total_ms": self.started.elapsed().as_secs_f64() * 1000.0,
            "passes": self.counts.get("passes").copied().unwrap_or(0),
            "ms": self.stages,
            "counts": self.counts,
        });
        let mut text = line.to_string();
        text.push('\n');
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = file.write_all(text.as_bytes());
        }
    }
}

tokio::task_local! {
    static CURRENT: RefCell<Option<CallTimer>>;
}

/// Run `fut` with a call timer when timing is on; `fut` itself when it is off.
pub(crate) async fn scope<F: Future>(query: &str, fut: F) -> F::Output {
    if !enabled() {
        return fut.await;
    }
    CURRENT
        .scope(RefCell::new(Some(CallTimer::new(query))), async {
            let out = fut.await;
            let _ = CURRENT.try_with(|cell| {
                if let Some(timer) = cell.borrow_mut().take() {
                    timer.finish();
                }
            });
            out
        })
        .await
}

fn with_timer(f: impl FnOnce(&mut CallTimer)) {
    if !enabled() {
        return;
    }
    let _ = CURRENT.try_with(|cell| {
        if let Some(timer) = cell.borrow_mut().as_mut() {
            f(timer);
        }
    });
}

/// Add the time since `started` to stage `name` of the current call.
pub(crate) fn stage(name: &'static str, started: Instant) {
    with_timer(|timer| timer.stage(name, started));
}

/// Add `n` to counter `name` of the current call.
pub(crate) fn count(name: &'static str, n: u64) {
    with_timer(|timer| timer.count(name, n));
}

/// Keep the largest `n` seen for counter `name` in the current call.
pub(crate) fn max_count(name: &'static str, n: u64) {
    with_timer(|timer| timer.max_count(name, n));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn f2a_stage_timing_off_writes_nothing() {
        // CSR_STAGE_TIMING_FILE is not set for the unit-test process.
        assert!(!enabled());
        stage("embed", Instant::now());
        count("passes", 1);
        max_count("ce_max_tokens", 9);
        let out = scope("anything", async { 7 }).await;
        assert_eq!(out, 7);
        assert!(CURRENT.try_with(|_| ()).is_err());
    }

    #[test]
    fn f2a_timer_accumulates_stages_and_counts() {
        let mut timer = CallTimer::new("query");
        timer.stage("hnsw", Instant::now());
        timer.stage("hnsw", Instant::now());
        timer.count("passes", 1);
        timer.count("passes", 1);
        timer.max_count("ce_max_tokens", 5);
        timer.max_count("ce_max_tokens", 3);
        assert_eq!(timer.counts["passes"], 2);
        assert_eq!(timer.counts["ce_max_tokens"], 5);
        assert!(timer.stages["hnsw"] >= 0.0);
        assert_eq!(timer.query_hash.len(), 16);
    }
}
