//! csr_why replay — env-only, runs the real `mcp::tools::why` (reinstatement walk
//! plus the rendered evidence chain and receipt) against a scratch DB clone and
//! prints the result per query.
//!
//! Guards, same as `hook_recall_replay`: the DB must canonicalize under
//! /tmp/csr-recall and must never be the live store. `why` logs retrieval events,
//! so run it on a throwaway `cp -c` clone, never on the golden snapshot.
//!
//! Required env:
//!   WHYREPLAY_DB        scratch DB path (index-v3 is read from its directory)
//!   WHYREPLAY_PROJECTS  Engine::new's projects dir (an empty scratch dir)
//!   WHYREPLAY_QUERIES   queries separated by newlines
//! Optional:
//!   WHYREPLAY_PROJECT   project scope handed to `why` (default
//!                       claude-self-reflect; "all" for unscoped)
//!   WHYREPLAY_TOP       evidence lines printed in the top-k summary (default 5)
//!
//! Run:
//!   WHYREPLAY_DB=/tmp/csr-recall/x/db.sqlite WHYREPLAY_PROJECTS=/tmp/csr-recall/x/projects \
//!   WHYREPLAY_QUERIES='why are companion dreams on by default instead of opt-in' \
//!     cargo run --release --example why_replay

use std::path::{Path, PathBuf};

use csr_engine::engine::Engine;
use csr_engine::mcp::tools::why;
use csr_engine::search::reinstatement::ReinstateConfig;

const LIVE_DB_MARKER: &str = ".claude-self-reflect";
const SCRATCH_ROOT: &str = "/tmp/csr-recall";

fn require_env(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("{name} is required"))
}

fn guard_db(raw: &str) -> Result<PathBuf, String> {
    if raw.contains(LIVE_DB_MARKER) {
        return Err(format!(
            "WHYREPLAY_DB ({raw}) contains '{LIVE_DB_MARKER}' - refusing to touch the live store"
        ));
    }
    let canonical = PathBuf::from(raw)
        .canonicalize()
        .map_err(|e| format!("WHYREPLAY_DB ({raw}) cannot be canonicalized: {e}"))?;
    let root = Path::new(SCRATCH_ROOT);
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    if !canonical.starts_with(&root) {
        return Err(format!(
            "WHYREPLAY_DB canonicalizes to {} which is not under {SCRATCH_ROOT}",
            canonical.display()
        ));
    }
    Ok(canonical)
}

#[tokio::main]
async fn main() {
    if let Err(msg) = run().await {
        eprintln!("why_replay: {msg}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let db = guard_db(&require_env("WHYREPLAY_DB")?)?;
    let projects = PathBuf::from(require_env("WHYREPLAY_PROJECTS")?);
    let queries: Vec<String> = require_env("WHYREPLAY_QUERIES")?
        .lines()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(str::to_string)
        .collect();
    if queries.is_empty() {
        return Err("WHYREPLAY_QUERIES has no query".into());
    }
    let project =
        std::env::var("WHYREPLAY_PROJECT").unwrap_or_else(|_| "claude-self-reflect".into());
    let top: usize = std::env::var("WHYREPLAY_TOP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);

    let engine = Engine::new(&db, &projects).map_err(|e| format!("Engine::new failed: {e}"))?;
    let cfg = ReinstateConfig::default();
    for query in &queries {
        let rendered = why(
            engine.storage(),
            engine.embeddings(),
            engine.search(),
            query,
            Some(&project),
            &cfg,
        )
        .await
        .map_err(|e| format!("why failed for {query:?}: {e}"))?;
        println!("===== QUERY: {query}");
        println!("--- top {top} evidence lines");
        for (rank, line) in rendered
            .lines()
            .filter(|l| l.trim_start().starts_with("best_via="))
            .take(top)
            .enumerate()
        {
            println!("{}. {}", rank + 1, line.trim());
        }
        println!("--- full rendering");
        println!("{rendered}");
    }
    Ok(())
}
