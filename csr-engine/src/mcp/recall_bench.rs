//! Warm-latency harness for `csr_reflect_on_past` and `csr_why`.
//!
//! `#[ignore]`: it needs a real corpus. Run it on a copy of a data directory:
//!
//! ```text
//! CSR_RECALL_BENCH_DB=/path/to/copy/csr-engine.db \
//! CSR_RECALL_BENCH_OUT=/path/to/out \
//! CSR_STAGE_TIMING_FILE=/path/to/out/stages.ndjson \
//! cargo test --release --lib recall_bench -- --ignored --nocapture
//! ```
//!
//! It loads the engine once, warms the model, index and statement caches with
//! queries that are not in the measured set, then times each measured query
//! through the same `tools::reflect_on_past` / `tools::why` functions the MCP
//! tools call. Every page is written to the out directory (timing stamps
//! stripped) so a before and an after run can be diffed, and the top-5 ids are
//! printed one line per query.

use std::path::PathBuf;
use std::time::Instant;

use crate::engine::Engine;
use crate::mcp::{stage_timing, tools};

const WARMUP: [&str; 3] = [
    "how does the watcher import new transcripts",
    "why does the hook inject a recap",
    "sqlite busy timeout on the daemon",
];

const QUERIES: [&str; 10] = [
    "why did we drop the Python and Docker stack for a Rust binary",
    "how does the pre-commit hook gate the release",
    "what broke when the MCP server advertised protocol version 2026-07-28",
    "dream verdicts stale anchor witness ledger",
    "recap paragraph at session start fallback to fragments",
    "reranker numerics differ between M3 and M5",
    "fork pull request merge blocked by CodeQL ruleset",
    "HNSW tombstones and over-fetch in search_index",
    "narrative generation claude -p usage budget",
    "transcript mirror lost sessions",
];

fn ids_in(page: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = page;
    while let Some(start) = rest.find("<id>") {
        let after = &rest[start + 4..];
        let Some(end) = after.find("</id>") else {
            break;
        };
        out.push(after[..end].to_string());
        rest = &after[end + 5..];
    }
    out
}

fn untimed(page: &str) -> String {
    let perf = regex::Regex::new(r"(?s)<perf>.*?</perf>|⚡ PERFORMANCE: \d+ms").unwrap();
    perf.replace_all(page, "").into_owned()
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx]
}

fn stage_lines(path: &Option<PathBuf>) -> Vec<serde_json::Value> {
    path.as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| {
            t.lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs CSR_RECALL_BENCH_DB, a copy of a real corpus"]
async fn recall_bench() {
    let db = PathBuf::from(std::env::var("CSR_RECALL_BENCH_DB").expect("CSR_RECALL_BENCH_DB"));
    let out = PathBuf::from(std::env::var("CSR_RECALL_BENCH_OUT").expect("CSR_RECALL_BENCH_OUT"));
    std::fs::create_dir_all(&out).unwrap();
    let rounds: usize = std::env::var("CSR_RECALL_BENCH_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let timing_file = std::env::var_os("CSR_STAGE_TIMING_FILE").map(PathBuf::from);
    assert!(stage_timing::enabled(), "set CSR_STAGE_TIMING_FILE");

    let projects = db.parent().unwrap().join("projects");
    std::fs::create_dir_all(&projects).unwrap();
    let load = Instant::now();
    let engine = Engine::new(&db, &projects).unwrap();
    println!("engine_load_ms {}", load.elapsed().as_millis());
    let (s, e, x) = (engine.storage(), engine.embeddings(), engine.search());
    let cfg = crate::search::reinstatement::ReinstateConfig {
        k: 10,
        ..Default::default()
    };

    for q in WARMUP {
        tools::reflect_on_past(s, e, x, q, 5, 0.3, None)
            .await
            .unwrap();
        tools::why(s, e, x, q, None, &cfg).await.unwrap();
    }

    let only = std::env::var("CSR_RECALL_BENCH_TOOLS").unwrap_or_default();
    for tool in ["reflect", "why"] {
        if !only.is_empty() && !only.split(",").any(|t| t == tool) {
            continue;
        }
        let mut walls: Vec<f64> = Vec::new();
        let mut stage_sum: std::collections::BTreeMap<String, Vec<f64>> = Default::default();
        for round in 0..rounds {
            for (i, q) in QUERIES.iter().enumerate() {
                let before = stage_lines(&timing_file).len();
                let started = Instant::now();
                let page = if tool == "reflect" {
                    tools::reflect_on_past(s, e, x, q, 5, 0.3, None)
                        .await
                        .unwrap()
                } else {
                    tools::why(s, e, x, q, None, &cfg).await.unwrap()
                };
                let wall = started.elapsed().as_secs_f64() * 1000.0;
                walls.push(wall);
                let lines = stage_lines(&timing_file);
                if let Some(line) = lines.get(before) {
                    if let Some(ms) = line["ms"].as_object() {
                        for (k, v) in ms {
                            stage_sum
                                .entry(k.clone())
                                .or_default()
                                .push(v.as_f64().unwrap_or(0.0));
                        }
                    }
                }
                let page = untimed(&page);
                if round == 0 {
                    std::fs::write(out.join(format!("{tool}-{i}.txt")), &page).unwrap();
                    let top: Vec<String> = ids_in(&page).into_iter().take(5).collect();
                    println!("IDS {tool} {i} {}", top.join(","));
                }
                println!("WALL {tool} {i} round{round} {wall:.1}");
            }
        }
        walls.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!(
            "SUMMARY {tool} n={} p50={:.1} p95={:.1} max={:.1}",
            walls.len(),
            percentile(&walls, 0.5),
            percentile(&walls, 0.95),
            walls[walls.len() - 1]
        );
        for (k, mut v) in stage_sum {
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            println!(
                "STAGE {tool} {k} n={} p50={:.1} max={:.1}",
                v.len(),
                percentile(&v, 0.5),
                v[v.len() - 1]
            );
        }
    }
}
