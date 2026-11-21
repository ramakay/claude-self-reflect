//! Nothing dead stays in `index-v3`.
//!
//! Once a generation is built, verified and serving, two kinds of files are
//! dead weight, and the daemon's regeneration tick (`engine::regen_tick`) calls
//! [`sweep`] to remove them:
//!
//! - **The index-v3 chunk graph** (`chunks-<pid>-<n>.hnsw.*` plus the manifest's
//!   `chunk_id_map`). It is removed at once: it is older than any generation
//!   and a generation-served engine never opens it. If a generation later fails
//!   to open, the fallback serves the previous generation or answers keyword-only
//!   while a new one is built (`SearchEngine::load_after_failed_open`); it never
//!   rebuilds the graph. `CSR_NO_VECTOR_GEN=1` stays an explicit opt-in: with the
//!   graph gone the index-v3 path backfills it from SQLite at load, and the
//!   daemon, which does not tick under that switch, never retires it again.
//! - **The previous generation.** It is the rollback if the current one fails
//!   to open, so it is kept for [`PREVIOUS_GRACE_SECS`] after the current one
//!   began serving, and removed only after the current one passes a full hash
//!   check at that moment.
//!
//! [`storage_lines`] reports the directory by purpose, for `status`.

use std::collections::BTreeMap;
use std::path::Path;

use super::regen::{self, RegenState};
use super::{read_watermark, verify_files};
use crate::storage::Storage;

/// How long the previous generation is kept after the current one began
/// serving (wall time, scaled by the soak clock like the hourly verify).
pub const PREVIOUS_GRACE_SECS: i64 = 3600;

/// What one [`sweep`] removed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Swept {
    pub graph_files: usize,
    pub graph_bytes: u64,
    pub pruned: Vec<u64>,
}

/// Retire what a verified serving generation made dead. `serving` is the
/// generation the engine serves chunks from (None: it serves none, and nothing
/// is removed). Records when `serving` began serving in `state`. Idempotent and
/// safe after an interruption; a failure is logged and retried at the next tick.
pub fn sweep(
    storage: &Storage,
    index_dir: &Path,
    serving: Option<u64>,
    state: &mut RegenState,
    now: i64,
) -> Swept {
    let grace = crate::runtime::scaled_secs(PREVIOUS_GRACE_SECS);
    sweep_with_grace(storage, index_dir, serving, state, now, grace)
}

pub(crate) fn sweep_with_grace(
    storage: &Storage,
    index_dir: &Path,
    serving: Option<u64>,
    state: &mut RegenState,
    now: i64,
    grace: i64,
) -> Swept {
    let mut swept = Swept::default();
    let Some(serving) = serving else {
        return swept;
    };
    if state.generation != serving || state.serving_since_unix == 0 {
        state.serving_since_unix = now;
    }
    // A generation that failed to open or to verify is being repaired: its
    // rollback must stay where it is.
    if state.repair_pending || state.last_error.is_some() {
        return swept;
    }
    let Ok(Some(watermark)) = read_watermark(storage) else {
        return swept;
    };
    if watermark.generation != serving {
        return swept;
    }
    let graphs = crate::search::retire::chunk_graph_files(index_dir);
    let prune_due = now - state.serving_since_unix >= grace
        && regen::generations_with_files(index_dir)
            .iter()
            .any(|generation| *generation < serving);
    if graphs.is_empty() && !prune_due {
        return swept;
    }
    // Verified: the full hash check the generation swap relies on, now.
    if let Err(error) = verify_files(index_dir, &watermark) {
        tracing::warn!(%error, generation = serving, "serving generation fails its full check; keeping the fallback files");
        return swept;
    }
    let Ok(_operation) = storage.publication_lock(index_dir) else {
        return swept;
    };
    if !graphs.is_empty() {
        match crate::search::retire::retire_chunk_graph(index_dir) {
            Ok(done) => {
                tracing::info!(
                    files = done.files,
                    bytes = done.bytes,
                    id_map_entries = done.id_map_entries,
                    "retired the index-v3 chunk graph; a generation serves chunks"
                );
                swept.graph_files = done.files;
                swept.graph_bytes = done.bytes;
            }
            Err(error) => {
                tracing::warn!(%error, "retiring the index-v3 chunk graph failed; retrying at the next tick");
            }
        }
    }
    if prune_due {
        // A build in flight holds this lock: leave the pruning to the next tick.
        if let Ok(_build) = regen::try_build_lock(index_dir) {
            swept.pruned = regen::prune_generations_below(index_dir, serving);
            if !swept.pruned.is_empty() {
                tracing::info!(
                    generations = ?swept.pruned,
                    serving,
                    "pruned the previous vector generation after its grace period"
                );
            }
        }
    }
    swept
}

fn human(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.2} GB", bytes as f64 / 1e9)
    } else {
        format!("{:.0} MB", bytes as f64 / 1e6)
    }
}

fn local_time(unix: i64) -> String {
    use chrono::TimeZone;
    chrono::Utc
        .timestamp_opt(unix, 0)
        .single()
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "an unknown time".to_string())
}

/// Bytes in `index_dir` by purpose.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct IndexUsage {
    /// Bytes per vector generation number.
    pub generations: BTreeMap<u64, u64>,
    /// The reflection graph the manifest names, plus the manifest.
    pub reflections: u64,
    /// Graph files nothing serves or names: the retired chunk graph, stragglers.
    pub dead: u64,
    pub dead_files: usize,
}

#[derive(serde::Deserialize)]
struct ManifestNames {
    reflection_basename: Option<String>,
}

pub fn measure(index_dir: &Path) -> IndexUsage {
    let reflection_basename = std::fs::read_to_string(index_dir.join("manifest.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<ManifestNames>(&raw).ok())
        .and_then(|names| names.reflection_basename)
        .unwrap_or_else(|| "reflections".to_string());
    let live = [
        format!("{reflection_basename}.hnsw.data"),
        format!("{reflection_basename}.hnsw.graph"),
    ];
    let mut usage = IndexUsage::default();
    for entry in std::fs::read_dir(index_dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
        let generation = name
            .strip_prefix("chunks-g")
            .and_then(|rest| rest.split('.').next())
            .and_then(|digits| digits.parse::<u64>().ok());
        if let Some(generation) = generation {
            *usage.generations.entry(generation).or_default() += bytes;
        } else if name.ends_with(".hnsw.data") || name.ends_with(".hnsw.graph") {
            if live.contains(&name) {
                usage.reflections += bytes;
            } else {
                usage.dead += bytes;
                usage.dead_files += 1;
            }
        } else if name == "manifest.json" {
            usage.reflections += bytes;
        }
    }
    usage
}

/// The directory by purpose, one line each, for `status`: serving generation,
/// previous generation and when it goes, reflections, and what is dead.
pub fn storage_lines(index_dir: &Path) -> Vec<String> {
    let usage = measure(index_dir);
    let state = regen::read_state(index_dir);
    let newest = usage.generations.keys().next_back().copied();
    let serving = match &state {
        Some(state) if state.generation > 0 => Some(state.generation),
        Some(_) => None,
        None => newest,
    };
    let total: u64 = usage.generations.values().sum::<u64>() + usage.reflections + usage.dead;
    let mut lines = vec![format!("index-v3 storage: {}", human(total))];
    let grace = crate::runtime::scaled_secs(PREVIOUS_GRACE_SECS);
    match (serving, newest) {
        (Some(generation), _) => {
            let bytes = usage.generations.get(&generation).copied().unwrap_or(0);
            lines.push(format!("  serving: g{generation}, {}", human(bytes)));
        }
        (None, Some(newest)) if state.as_ref().is_some_and(|s| s.repair_pending) => {
            lines.push(format!(
                "  serving: none; g{newest} will not open and a new generation is being built (chunk search is keyword-only until it serves)"
            ));
        }
        (None, Some(newest)) => {
            lines.push(format!(
                "  serving: none yet; the daemon switches to g{newest} at its next tick"
            ));
        }
        (None, None) => {
            lines.push("  serving: no vector generation built yet".to_string());
        }
    }
    let since = state.as_ref().map_or(0, |s| s.serving_since_unix);
    let mut previous = false;
    for (generation, bytes) in &usage.generations {
        match serving {
            Some(serving) if *generation < serving => {
                previous = true;
                let until = if since > 0 {
                    format!("until {}", local_time(since + grace))
                } else {
                    "until it has been verified and the new one has served for the grace period"
                        .to_string()
                };
                lines.push(format!(
                    "  previous: g{generation}, {}, kept {until}",
                    human(*bytes)
                ));
            }
            Some(serving) if *generation > serving => {
                lines.push(format!(
                    "  newer: g{generation}, {}, waiting to be served",
                    human(*bytes)
                ));
            }
            _ => {}
        }
    }
    if !previous {
        lines.push("  previous: none kept".to_string());
    }
    lines.push(format!("  reflections: {}", human(usage.reflections)));
    if usage.dead_files == 0 {
        lines.push("  dead: none".to_string());
    } else {
        lines.push(format!(
            "  dead: {} in {} files (removed at the next daemon tick once a generation serves)",
            human(usage.dead),
            usage.dead_files
        ));
    }
    lines
}
