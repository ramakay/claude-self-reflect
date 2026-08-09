//! Status subcommand — fast system health check without EmbeddingEngine.
//!
//! `csr-engine status`            — JSON output (for tooling)
//! `csr-engine status --compact`  — One-line for statusline

use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use crate::import;
use crate::storage::Storage;

/// Status data gathered from SQLite and disk.
#[derive(Serialize)]
pub struct StatusReport {
    pub conversations: usize,
    pub projects: usize,
    pub chunks: usize,
    pub reflections: usize,
    pub imported_files: usize,
    pub total_jsonl_files: usize,
    pub import_percent: f64,
    /// Backward-compatible sum of the two split suppression counters below.
    pub csr_self_suppressed: i64,
    pub csr_tool_blocks_suppressed: i64,
    pub csr_hook_wrappers_scrubbed: i64,
    pub enrichment: EnrichmentBreakdown,
    pub narratives: NarrativeStatus,
    pub ratification: RatificationStatus,
    pub newest_chunk: Option<String>,
    pub db_size_bytes: u64,
    pub db_path: String,
    pub healthy: bool,
    /// True when a live MCP server is running an older build than the binary now
    /// on disk — the connection must be re-established for the upgrade to apply.
    pub mcp_binary_stale: bool,
    /// Aux corpus coverage (session_registry vs chunks) — never injected into search.
    pub aux: AuxStatus,
    /// v10 "dreaming" summary (`crate::dream`) — witness_verdicts totals and
    /// current demoted-symbol count. See `gather_dream`.
    pub dream: DreamStatus,
}

/// Totals by verdict kind across every `witness_verdicts` event ever
/// recorded (not latest-per-witness — see
/// `storage::witness_verdicts::event_totals_by_verdict`).
#[derive(Serialize, Default, Debug, PartialEq, Eq)]
pub struct DreamVerdictTotals {
    pub obsolete: i64,
    pub superseded: i64,
    pub reinstated: i64,
}

/// v10 "dreaming" summary block. `last_run` is the `created_at` timestamp of
/// the globally newest `witness_verdicts` event (`None` if `dream` has never
/// run). `demoted_symbols` is the count on the `Demote` channel right now —
/// see `storage::witness_verdicts::all_demoted_symbols`.
///
/// `last_daemon_run`/`next_due` are the DAEMON's own cadence bookkeeping
/// (`daemon::dream_cadence`) — deliberately separate from `last_run`: a
/// daemon cycle that runs and writes zero new events (re-running at an
/// unchanged HEAD) still counts as "the daemon acted" and moves cadence
/// forward, but would leave `last_run` frozen since no event was written.
#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct DreamStatus {
    /// Whether daemon dreaming is enabled by configuration.
    pub daemon_enabled: bool,
    pub last_run: Option<String>,
    pub events_total: i64,
    pub by_verdict: DreamVerdictTotals,
    pub demoted_symbols: i64,
    /// Total durable witnesses available for a first dream cycle.
    pub witnesses_ledgered: i64,
    /// Evidence-backed conversations carrying a release-ancestry cache row.
    pub ancestry_cached_conversations: i64,
    /// RFC3339 timestamp of the daemon's last COMPLETED dream cycle. `None`
    /// if the daemon dream loop has never completed one (never started,
    /// disabled via `CSR_NO_DREAMING`, or still waiting on its first
    /// cadence/catch-up window).
    pub last_daemon_run: Option<String>,
    /// RFC3339 timestamp of the next expected daemon cycle
    /// (`last_daemon_run + interval`). `None` when `last_daemon_run` is
    /// `None` or `daemon_enabled` is false — the exact first-cycle timing
    /// depends on daemon process-start state a stateless status read doesn't
    /// have (see `daemon::dream_cadence::first_cycle_due_at`).
    pub next_due: Option<String>,
}

impl Default for DreamStatus {
    fn default() -> Self {
        Self {
            daemon_enabled: true,
            last_run: None,
            events_total: 0,
            by_verdict: DreamVerdictTotals::default(),
            demoted_symbols: 0,
            witnesses_ledgered: 0,
            ancestry_cached_conversations: 0,
            last_daemon_run: None,
            next_due: None,
        }
    }
}

#[derive(Serialize, Default, Debug, PartialEq, Eq)]
pub struct CoverageStats {
    pub sessions_seen: i64,
    pub sessions_imported: i64,
    pub gap: i64,
}

#[derive(Serialize, Default, Debug, PartialEq, Eq)]
pub struct SchemaMissCounts {
    pub tasks: i64,
    pub plans: i64,
    pub history: i64,
}

/// Positive per-source corpus counts (v9.4 multi-source adapters).
/// `schema_misses` says what failed to parse; this says what actually landed.
#[derive(Serialize, Default, Debug, PartialEq, Eq)]
pub struct SourceCounts {
    pub plan_docs: i64,
    pub plan_chunks: i64,
    pub plan_unscoped_docs: i64,
    pub registry_sessions: i64,
    pub task_sessions_on_disk: usize,
    pub resolution_proposals: i64,
    pub resolution_verdicts: i64,
}

#[derive(Serialize, Default, Debug, PartialEq, Eq)]
pub struct AuxStatus {
    pub coverage: CoverageStats,
    pub file_history_sessions: usize,
    pub transcripts_unindexed: usize,
    pub schema_misses: SchemaMissCounts,
    pub sources: SourceCounts,
}

#[derive(Serialize, Default)]
pub struct EnrichmentBreakdown {
    pub heuristic_completed: usize,
    pub heuristic_failed: usize,
    pub extracted_v3_completed: usize,
    pub extracted_v3_failed: usize,
    pub ai_narrative_completed: usize,
    pub ai_narrative_failed: usize,
    pub ai_narrative_processing: usize,
}

#[derive(Serialize, Default)]
pub struct NarrativeStatus {
    pub calls_today: i64,
    pub tokens_today: i64,
    pub calls_total: i64,
    pub tokens_total: i64,
    pub cache_tokens_today: i64,
    pub cache_tokens_total: i64,
    pub last_model: Option<String>,
    pub disabled: bool,
}

#[derive(Serialize, Default)]
pub struct RatificationStatus {
    pub count: i64,
    pub avg_score: f64,
    pub coverage_pct: f64, // scored / distinct conversation_ids in chunks table, 0-100
}

/// Run status check — opens SQLite directly (no EmbeddingEngine needed).
/// `deep` forces a fresh `PRAGMA integrity_check` (~10s on multi-GB DBs);
/// otherwise the cached verdict is served.
pub fn handle(
    db_path: &Path,
    projects_dir: &Path,
    compact: bool,
    swiftbar: bool,
    deep: bool,
) -> Result<()> {
    let report = gather_status(db_path, projects_dir, deep)?;

    if swiftbar {
        print_swiftbar(&report);
    } else if compact {
        print_compact(&report);
    } else {
        println!("{}", serde_json::to_string_pretty(&report)?);
    }

    Ok(())
}

/// Public wrapper for `gather_status` — used by the telemetry module to embed
/// a fresh status snapshot in its report without re-implementing the gather logic.
pub fn gather_status_public(db_path: &Path, projects_dir: &Path) -> Result<StatusReport> {
    gather_status(db_path, projects_dir, false)
}

/// Gather status data from SQLite and disk.
fn gather_status(db_path: &Path, projects_dir: &Path, deep: bool) -> Result<StatusReport> {
    // Count total JSONL files on disk
    let total_jsonl = count_jsonl_files(projects_dir);

    // If DB doesn't exist yet, return empty report
    if !db_path.exists() {
        return Ok(StatusReport {
            mcp_binary_stale: crate::binary_stamp::serving_binary_is_stale(),
            conversations: 0,
            projects: 0,
            chunks: 0,
            reflections: 0,
            imported_files: 0,
            total_jsonl_files: total_jsonl,
            import_percent: 0.0,
            csr_self_suppressed: 0,
            csr_tool_blocks_suppressed: 0,
            csr_hook_wrappers_scrubbed: 0,
            enrichment: EnrichmentBreakdown::default(),
            narratives: NarrativeStatus {
                disabled: crate::narrative::narratives_disabled(),
                ..Default::default()
            },
            ratification: RatificationStatus::default(),
            newest_chunk: None,
            db_size_bytes: 0,
            db_path: db_path.to_string_lossy().to_string(),
            healthy: false,
            aux: AuxStatus::default(),
            dream: DreamStatus::default(),
        });
    }

    let storage = Storage::open(db_path)?;
    let narratives = gather_narratives(&storage);

    let conversations = storage.count_conversations().unwrap_or(0);
    let ratification = gather_ratification(&storage, conversations);
    let projects = storage.count_projects().unwrap_or(0);
    let chunks = storage.count_chunk_embeddings().unwrap_or(0);
    let reflections = storage.count_reflection_embeddings().unwrap_or(0);
    let imported_files = storage.count_imported_files().unwrap_or(0);
    let csr_self_suppressed = storage.get_csr_self_suppressed().unwrap_or(0);
    let csr_tool_blocks_suppressed = storage.get_csr_tool_blocks_suppressed().unwrap_or(0);
    let csr_hook_wrappers_scrubbed = storage.get_csr_hook_wrappers_scrubbed().unwrap_or(0);
    let newest_chunk = storage.get_newest_chunk_timestamp().unwrap_or(None);
    let db_size_bytes = storage.get_db_size().unwrap_or(0);
    // Cached verdict (24h TTL, refreshed by the daemon or --deep). A full
    // integrity_check costs ~10s of CPU on a multi-GB DB — must never run on
    // the statusline path.
    // ttl=0 + refresh forces a fresh check (and stores it); otherwise serve cache.
    let healthy = if deep {
        storage.integrity_check_cached(0, true).unwrap_or(false)
    } else {
        storage.integrity_check_cached(24, false).unwrap_or(false)
    };

    let import_percent = if total_jsonl > 0 {
        (imported_files as f64 / total_jsonl as f64 * 100.0).min(100.0)
    } else {
        0.0
    };

    // Enrichment breakdown
    let mut enrichment = EnrichmentBreakdown::default();
    if let Ok(breakdown) = storage.get_enrichment_breakdown() {
        for (etype, status, count) in &breakdown {
            match (etype.as_str(), status.as_str()) {
                ("heuristic", "completed") => enrichment.heuristic_completed = *count,
                ("heuristic", "failed") => enrichment.heuristic_failed = *count,
                ("extracted_v3", "completed") => enrichment.extracted_v3_completed = *count,
                ("extracted_v3", "failed") => enrichment.extracted_v3_failed = *count,
                ("ai_narrative", "completed") => enrichment.ai_narrative_completed = *count,
                ("ai_narrative", "failed") => enrichment.ai_narrative_failed = *count,
                ("ai_narrative", "processing") => enrichment.ai_narrative_processing = *count,
                _ => {}
            }
        }
    }

    let aux = gather_aux(&storage, projects_dir);
    let dream = gather_dream(&storage);

    Ok(StatusReport {
        mcp_binary_stale: crate::binary_stamp::serving_binary_is_stale(),
        conversations,
        projects,
        chunks,
        reflections,
        imported_files,
        total_jsonl_files: total_jsonl,
        import_percent,
        csr_self_suppressed,
        csr_tool_blocks_suppressed,
        csr_hook_wrappers_scrubbed,
        enrichment,
        narratives,
        ratification,
        newest_chunk,
        db_size_bytes,
        db_path: db_path.to_string_lossy().to_string(),
        healthy,
        aux,
        dream,
    })
}

/// Assemble the v10 "dreaming" status block. Fail-soft to defaults —
/// `status` opens SQLite directly and must never fail on a pre-migration
/// schema gap (mirrors `gather_narratives`).
fn gather_dream(storage: &Storage) -> DreamStatus {
    gather_dream_with(
        storage,
        crate::storage::recap_feeds::dream_consumption_mode(),
    )
}

fn gather_dream_with(
    storage: &Storage,
    consumption_mode: crate::storage::recap_feeds::ConsumptionMode,
) -> DreamStatus {
    let last_run = storage
        .last_dream_run()
        .unwrap_or(None)
        .map(|(_head_oid, created_at)| created_at);
    // I6: ConsumptionMode defaults to AnnotateOnly — verdict counts are
    // user-facing by default; only an explicit Off suppresses them, and the
    // Demote-channel forgotten count additionally requires Full. The
    // daemon's own operational bookkeeping below (whether it's enabled, when
    // it last ran, when it's next due) is NOT verdict content and stays
    // visible in every mode; gating skips the underlying queries entirely
    // rather than computing real totals and hiding them.
    let (obsolete, superseded, reinstated) =
        if consumption_mode != crate::storage::recap_feeds::ConsumptionMode::Off {
            storage.dream_event_totals().unwrap_or((0, 0, 0))
        } else {
            (0, 0, 0)
        };
    let demoted_symbols = if consumption_mode == crate::storage::recap_feeds::ConsumptionMode::Full
    {
        storage
            .all_demoted_symbols()
            .map(|v| v.len() as i64)
            .unwrap_or(0)
    } else {
        0
    };
    let last_daemon_run_dt = crate::daemon::dream_cadence::read_last_run(storage);
    let last_daemon_run = last_daemon_run_dt.map(|t| t.to_rfc3339());
    let daemon_enabled = !crate::daemon::dream_cadence::dreaming_disabled();
    // The TUI only renders this count for the enabled/never-run state, so keep
    // the extra COUNT query off the steady-state refresh path.
    let witnesses_ledgered = if daemon_enabled && last_daemon_run_dt.is_none() {
        storage
            .with_connection(crate::storage::witness_ledger::count_all)
            .unwrap_or(0)
    } else {
        0
    };
    let next_due = daemon_enabled
        .then(|| {
            crate::daemon::dream_cadence::next_due(
                last_daemon_run_dt,
                crate::daemon::dream_cadence::interval_secs(),
            )
        })
        .flatten()
        .map(|t| t.to_rfc3339());

    DreamStatus {
        daemon_enabled,
        last_run,
        events_total: obsolete + superseded + reinstated,
        by_verdict: DreamVerdictTotals {
            obsolete,
            superseded,
            reinstated,
        },
        demoted_symbols,
        witnesses_ledgered,
        ancestry_cached_conversations: storage.ancestry_cache_count().unwrap_or(0),
        last_daemon_run,
        next_due,
    }
}

/// Assemble aux coverage / file-history / transcript gap stats. Fail-soft to zeros.
fn gather_aux(storage: &Storage, projects_dir: &Path) -> AuxStatus {
    let (seen, imported, gap) = storage.coverage_stats().unwrap_or((0, 0, 0));
    let claude_dir = projects_dir.parent();

    let file_history_sessions = claude_dir
        .map(|d| d.join("file-history"))
        .filter(|p| p.is_dir())
        .and_then(|p| std::fs::read_dir(p).ok())
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .count()
        })
        .unwrap_or(0);

    let transcripts_unindexed = count_unindexed_transcripts(storage, claude_dir);

    let counters = storage.get_aux_counters().unwrap_or_default();
    let mut schema_misses = SchemaMissCounts::default();
    for (source, count) in counters {
        match source.as_str() {
            "tasks" => schema_misses.tasks = count,
            "plans" => schema_misses.plans = count,
            "history" => schema_misses.history = count,
            _ => {}
        }
    }

    let (plan_docs, plan_chunks, plan_unscoped_docs) =
        storage.plan_source_counts().unwrap_or((0, 0, 0));
    let task_sessions_on_disk = claude_dir
        .map(|d| d.join("tasks"))
        .filter(|p| p.is_dir())
        .and_then(|p| std::fs::read_dir(p).ok())
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .count()
        })
        .unwrap_or(0);

    AuxStatus {
        coverage: CoverageStats {
            sessions_seen: seen,
            sessions_imported: imported,
            gap,
        },
        file_history_sessions,
        transcripts_unindexed,
        schema_misses,
        sources: SourceCounts {
            plan_docs,
            plan_chunks,
            plan_unscoped_docs,
            registry_sessions: seen,
            task_sessions_on_disk,
            resolution_proposals: storage.count_resolution_proposals().unwrap_or(0),
            resolution_verdicts: storage.count_resolution_verdicts().unwrap_or(0),
        },
    }
}

fn count_unindexed_transcripts(storage: &Storage, claude_dir: Option<&Path>) -> usize {
    let dir = match claude_dir.map(|d| d.join("transcripts")) {
        Some(d) if d.is_dir() => d,
        _ => return 0,
    };
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    let candidates: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                return None;
            }
            let stem = path.file_stem()?.to_str()?.to_string();
            // Only ses_*.jsonl per spec.
            if !stem.starts_with("ses_") {
                return None;
            }
            Some(stem.strip_prefix("ses_").unwrap_or(&stem).to_string())
        })
        .collect();
    if candidates.is_empty() {
        return 0;
    }
    let known = storage.known_session_ids(&candidates).unwrap_or_default();
    candidates.iter().filter(|c| !known.contains(*c)).count()
}

/// Aggregate narrative_usage via the existing Storage helper. Tolerates the
/// table not existing (pre-migration DBs): status opens SQLite directly via
/// Storage::open and must never fail on schema gaps.
fn gather_narratives(storage: &Storage) -> NarrativeStatus {
    let disabled = crate::narrative::narratives_disabled();
    let summary = storage.narrative_usage_summary().unwrap_or_default();
    NarrativeStatus {
        calls_today: summary.calls_today,
        tokens_today: summary.tokens_today,
        calls_total: summary.calls_total,
        tokens_total: summary.tokens_total,
        cache_tokens_today: summary.cache_tokens_today,
        cache_tokens_total: summary.cache_tokens_total,
        last_model: summary.last_model,
        disabled,
    }
}

fn gather_ratification(storage: &Storage, total_conversations: usize) -> RatificationStatus {
    let (count, avg_score) = storage.ratification_summary().unwrap_or((0, 0.0));
    let coverage_pct = if total_conversations > 0 {
        (count as f64 / total_conversations as f64) * 100.0
    } else {
        0.0
    };
    RatificationStatus {
        count,
        avg_score,
        coverage_pct,
    }
}

fn format_narrative_segment(n: &NarrativeStatus) -> String {
    if n.disabled {
        return "AI off".to_string();
    }
    if n.calls_today == 0 {
        return "AI 0c today".to_string();
    }
    let tok = if n.tokens_today >= 1000 {
        format!("{:.1}k", n.tokens_today as f64 / 1000.0)
    } else {
        n.tokens_today.to_string()
    };
    format!("AI {}c/{} tok today", n.calls_today, tok)
}

/// Count total JSONL files across all project directories.
fn count_jsonl_files(projects_dir: &Path) -> usize {
    let projects = match import::discover_projects(projects_dir) {
        Ok(p) => p,
        Err(_) => return 0,
    };

    let mut count = 0;
    for (dir, _) in &projects {
        if let Ok(files) = import::list_jsonl_files(dir) {
            count += files.len();
        }
    }
    count
}

/// Print SwiftBar-compatible output for macOS menu bar.
///
/// SwiftBar format:
/// - First line = menu bar title (always visible)
/// - `---` = separator between title and dropdown
/// - Subsequent lines = dropdown items
/// - `| color=...` = styling
/// - `| href=...` = clickable URL
/// - `--` prefix = submenu item
fn print_swiftbar(report: &StatusReport) {
    // Menu bar title: health indicator + chunk count
    let icon = if report.healthy { "🧠" } else { "⚠️" };
    let focus = read_current_focus().unwrap_or_default();
    if focus.is_empty() {
        println!("{} {}c {}r", icon, report.chunks, report.reflections);
    } else {
        // Truncate focus to 30 chars for menu bar
        let short: String = focus.chars().take(30).collect();
        println!("{} {}", icon, short);
    }

    println!("---");

    // Section: Index Stats
    println!("Index | size=14 color=#888888");
    println!("--Chunks: {} | font=Menlo", report.chunks);
    println!("--Reflections: {} | font=Menlo", report.reflections);
    println!("--Conversations: {} | font=Menlo", report.conversations);
    println!("--Projects: {} | font=Menlo", report.projects);

    // Section: Import Progress
    let bar_filled = (report.import_percent / 10.0).round() as usize;
    let bar_empty = 10_usize.saturating_sub(bar_filled);
    let bar: String = "█".repeat(bar_filled) + &"░".repeat(bar_empty);
    println!("---");
    println!(
        "Import: {} {:.0}% ({}/{}) | font=Menlo",
        bar, report.import_percent, report.imported_files, report.total_jsonl_files
    );

    // Section: Enrichment
    let total_enriched = report.enrichment.heuristic_completed
        + report.enrichment.extracted_v3_completed
        + report.enrichment.ai_narrative_completed;
    if total_enriched > 0 {
        println!("---");
        println!("Enrichment | size=14 color=#888888");
        println!(
            "--Heuristic: {} | font=Menlo",
            report.enrichment.heuristic_completed
        );
        println!(
            "--V3 Extraction: {} | font=Menlo",
            report.enrichment.extracted_v3_completed
        );
        println!(
            "--AI Narrative: {} | font=Menlo",
            report.enrichment.ai_narrative_completed
        );
        if report.enrichment.ai_narrative_processing > 0 {
            println!(
                "--Processing: {} | font=Menlo color=orange",
                report.enrichment.ai_narrative_processing
            );
        }
    }

    // Section: Health
    println!("---");
    let health_label = if report.healthy {
        "DB: healthy ✓ | color=green"
    } else {
        "DB: unhealthy ✗ | color=red"
    };
    println!("{}", health_label);
    let db_mb = report.db_size_bytes as f64 / 1_048_576.0;
    println!("Size: {:.1} MB | font=Menlo", db_mb);
    if let Some(ref ts) = report.newest_chunk {
        let age = format_age(ts);
        println!("Latest: {} | font=Menlo", age);
    }

    // Section: Current Focus (from session_start hook)
    if !focus.is_empty() {
        println!("---");
        println!("Focus: {} | font=Menlo", focus);
    }

    // Section: Quick Actions
    println!("---");
    println!("Refresh | refresh=true");
    println!(
        "Open DB in Terminal | bash=sqlite3 param1=\"{}\" terminal=true",
        report.db_path
    );
}

/// Read the current focus file written by session_start hook.
/// Strips framing prefixes like "[project] SESSION CONTINUITY..." to extract clean focus.
fn read_current_focus() -> Option<String> {
    let home = dirs::home_dir()?;
    let path = home.join(".claude-self-reflect").join("current-focus.txt");
    let content = std::fs::read_to_string(&path).ok()?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Strip "[project] " prefix if present
    let clean = if let Some(pos) = trimmed.find("] ") {
        &trimmed[pos + 2..]
    } else {
        trimmed
    };
    // Skip session framing noise
    let skip_prefixes = [
        "SESSION CONTINUITY",
        "RECENT SESSIONS",
        "CSR engine ready",
        "CSR:",
    ];
    if skip_prefixes.iter().any(|p| clean.starts_with(p)) {
        return None;
    }
    Some(clean.to_string())
}

/// Format a timestamp as a human-readable age string.
fn format_age(timestamp: &str) -> String {
    let ts = match crate::temporal::parse_timestamp(timestamp) {
        Some(t) => t,
        None => return timestamp.to_string(),
    };
    let age = chrono::Utc::now() - ts;
    let mins = age.num_minutes();
    if mins < 1 {
        "just now".to_string()
    } else if mins < 60 {
        format!("{}m ago", mins)
    } else if mins < 1440 {
        format!("{}h ago", mins / 60)
    } else {
        format!("{}d ago", mins / 1440)
    }
}

/// Print compact one-line status for statusline integration.
fn print_compact(report: &StatusReport) {
    print!("{}", format_compact(report));
}

/// Pure formatter for the compact one-line statusline — separated from
/// `print_compact` so tests can assert on the string directly (same split
/// `format_narrative_segment` uses).
fn format_compact(report: &StatusReport) -> String {
    // Format: [████████░░ 82%] [✓ 909c 54r] [3 projects]
    let bar_filled = (report.import_percent / 10.0).round() as usize;
    let bar_empty = 10_usize.saturating_sub(bar_filled);
    let bar: String = "█".repeat(bar_filled) + &"░".repeat(bar_empty);

    let health = if report.healthy { "ok" } else { "!!" };

    let mut out = format!(
        "[{} {:.0}%] [{}] {}c {}r | {}p | {}",
        bar,
        report.import_percent,
        health,
        report.conversations,
        report.reflections,
        report.projects,
        format_narrative_segment(&report.narratives),
    );
    // v10 "dreaming": only speak up when there's something to forget —
    // terse by design, matching the rest of this line's style.
    if report.dream.demoted_symbols > 0 {
        out.push_str(&format!(" | ☾ {} forgotten", report.dream.demoted_symbols));
    }
    // A newer binary is installed but the live MCP server predates it. Say so
    // on the statusline the user already watches, rather than leaving them to
    // discover it from stale behaviour.
    if report.mcp_binary_stale {
        out.push_str(" | ⟳ reconnect mcp");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compact_includes_narratives() {
        let s = format_narrative_segment(&NarrativeStatus {
            calls_today: 3,
            tokens_today: 12_400,
            calls_total: 120,
            tokens_total: 480_000,
            cache_tokens_today: 0,
            cache_tokens_total: 0,
            last_model: Some("claude-haiku-4-5".into()),
            disabled: false,
        });
        assert_eq!(s, "AI 3c/12.4k tok today");

        let off = format_narrative_segment(&NarrativeStatus {
            disabled: true,
            ..Default::default()
        });
        assert_eq!(off, "AI off");

        let idle = format_narrative_segment(&NarrativeStatus::default());
        assert_eq!(idle, "AI 0c today");
    }

    #[test]
    fn test_status_nonexistent_db() {
        let _guard = crate::daemon::dream_cadence::env_test_guard();
        let report = gather_status(
            Path::new("/tmp/nonexistent-csr-test.db"),
            Path::new("/tmp/nonexistent-projects"),
            false,
        )
        .unwrap();
        assert_eq!(report.conversations, 0);
        assert!(!report.healthy);
        assert_eq!(report.import_percent, 0.0);
    }

    fn base_report() -> StatusReport {
        StatusReport {
            mcp_binary_stale: false,
            conversations: 909,
            projects: 3,
            chunks: 5000,
            reflections: 54,
            imported_files: 909,
            total_jsonl_files: 1000,
            import_percent: 90.9,
            csr_self_suppressed: 0,
            csr_tool_blocks_suppressed: 0,
            csr_hook_wrappers_scrubbed: 0,
            enrichment: EnrichmentBreakdown::default(),
            narratives: NarrativeStatus::default(),
            ratification: RatificationStatus::default(),
            newest_chunk: None,
            db_size_bytes: 100_000_000,
            db_path: "/tmp/test.db".to_string(),
            healthy: true,
            aux: AuxStatus::default(),
            dream: DreamStatus::default(),
        }
    }

    #[test]
    fn test_compact_format() {
        // Just verify it doesn't panic
        print_compact(&base_report());
    }

    #[test]
    fn test_compact_omits_dream_suffix_when_nothing_forgotten() {
        let report = base_report();
        assert_eq!(report.dream.demoted_symbols, 0);
        let line = format_compact(&report);
        assert!(
            !line.contains('☾'),
            "no demoted symbols means no dream suffix: {line:?}"
        );
    }

    #[test]
    fn test_compact_includes_dream_suffix_when_symbols_forgotten() {
        let mut report = base_report();
        report.dream = DreamStatus {
            daemon_enabled: true,
            last_run: Some("2026-08-05 10:00:00".into()),
            events_total: 3,
            by_verdict: DreamVerdictTotals {
                obsolete: 2,
                superseded: 1,
                reinstated: 0,
            },
            demoted_symbols: 3,
            witnesses_ledgered: 0,
            ancestry_cached_conversations: 0,
            last_daemon_run: None,
            next_due: None,
        };
        let line = format_compact(&report);
        assert!(
            line.contains("☾ 3 forgotten"),
            "must surface the demoted count: {line:?}"
        );
    }

    #[test]
    fn test_status_with_empty_db() {
        let _guard = crate::daemon::dream_cadence::env_test_guard();
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let projects_dir = dir.path().join("projects");
        std::fs::create_dir_all(&projects_dir).unwrap();

        // Create the database first (status is read-only, won't create it)
        let _storage = Storage::open(&db_path).unwrap();

        let report = gather_status(&db_path, &projects_dir, false).unwrap();
        assert_eq!(report.conversations, 0);
        assert_eq!(report.chunks, 0);
        assert!(report.healthy); // empty DB passes integrity check
        assert_eq!(report.import_percent, 0.0);
    }

    #[test]
    fn status_aux_block_assembles_with_missing_dirs() {
        let _guard = crate::daemon::dream_cadence::env_test_guard();
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let projects_dir = dir.path().join("projects");
        std::fs::create_dir_all(&projects_dir).unwrap();
        // No sibling file-history/ or transcripts/ dirs.

        let _storage = Storage::open(&db_path).unwrap();
        let report = gather_status(&db_path, &projects_dir, false).unwrap();
        assert_eq!(report.aux.coverage, CoverageStats::default());
        assert_eq!(report.aux.file_history_sessions, 0);
        assert_eq!(report.aux.transcripts_unindexed, 0);
    }

    #[test]
    fn status_surfaces_split_csr_suppression_counters_and_sum() {
        let _guard = crate::daemon::dream_cadence::env_test_guard();
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let projects_dir = dir.path().join("projects");
        std::fs::create_dir_all(&projects_dir).unwrap();
        let storage = Storage::open(&db_path).unwrap();
        storage.set_meta("csr_tool_blocks_suppressed", "4").unwrap();
        storage.set_meta("csr_hook_wrappers_scrubbed", "2").unwrap();

        let value =
            serde_json::to_value(gather_status(&db_path, &projects_dir, false).unwrap()).unwrap();
        assert_eq!(value["csr_tool_blocks_suppressed"], 4);
        assert_eq!(value["csr_hook_wrappers_scrubbed"], 2);
        assert_eq!(value["csr_self_suppressed"], 6);
    }

    #[test]
    fn status_dream_block_defaults_to_empty_on_fresh_db() {
        // gather_dream reads the process-global CSR_NO_DREAMING kill switch;
        // hold the shared env lock so parallel kill-switch tests can't flip
        // daemon_enabled mid-assertion.
        let _guard = crate::daemon::dream_cadence::env_test_guard();
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let projects_dir = dir.path().join("projects");
        std::fs::create_dir_all(&projects_dir).unwrap();

        let _storage = Storage::open(&db_path).unwrap();
        let report = gather_status(&db_path, &projects_dir, false).unwrap();
        assert_eq!(report.dream, DreamStatus::default());
        assert_eq!(report.dream.demoted_symbols, 0);
        assert!(report.dream.last_run.is_none());
    }

    #[test]
    fn status_dream_block_surfaces_ancestry_cache_count() {
        let _guard = crate::daemon::dream_cadence::env_test_guard();
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let projects_dir = dir.path().join("projects");
        std::fs::create_dir_all(&projects_dir).unwrap();

        let storage = Storage::open(&db_path).unwrap();
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO conversation_ancestry_cache
                     (conversation_id, state, release_tag, releases_behind, repository, refreshed_at)
                     VALUES ('session-1', 'shipped', 'v1.0.0', 2, '/repo', '2026-08-06T12:00:00Z')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        drop(storage);

        let report = gather_status(&db_path, &projects_dir, false).unwrap();
        assert_eq!(report.dream.ancestry_cached_conversations, 1);
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["dream"]["ancestry_cached_conversations"], 1);
    }

    #[test]
    fn status_dream_block_shows_verdict_counts_by_default_and_suppresses_when_off() {
        let _guard = crate::daemon::dream_cadence::env_test_guard();
        use crate::storage::witness_ledger::WitnessLedgerRow;
        use crate::storage::witness_verdicts::{VerdictKind, WitnessVerdictRow};

        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let projects_dir = dir.path().join("projects");
        std::fs::create_dir_all(&projects_dir).unwrap();

        let storage = Storage::open(&db_path).unwrap();
        storage
            .insert_witness(&WitnessLedgerRow {
                id: 0,
                project: "proj".into(),
                file: "/repo/src/gone.rs".into(),
                symbol: Some("vanished".into()),
                span_start: Some(1),
                span_end: Some(3),
                stamp: "b3:1".into(),
                tier: "committed".into(),
                at_oid: Some("aaa".into()),
                source_kind: "backfill".into(),
                source_id: Some("aaa".into()),
            })
            .unwrap();
        let w = storage
            .witnesses_for_file("proj", "/repo/src/gone.rs")
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        storage
            .insert_witness_verdict(&WitnessVerdictRow {
                witness_id: w.id,
                verdict: VerdictKind::AnchorObsolete,
                successor_witness_id: None,
                receipt_oid: Some("headoid".into()),
                observed_head_oid: "headoid".into(),
            })
            .unwrap();
        drop(storage);

        // CSR_DREAM_CONSUMPTION is unset in this test process — the real
        // default, which since I6 is AnnotateOnly: verdict COUNTS are
        // visible (dreams are user-facing by default) while the Demote
        // channel stays dark. `gather_status` is the actual `csr-engine
        // status` production path with no test-only parameter seam, so this
        // drives it exactly as a real user would see it today.
        let report = gather_status(&db_path, &projects_dir, false).unwrap();
        assert_eq!(
            report.dream.events_total, 1,
            "verdict totals must be visible under the AnnotateOnly default"
        );
        assert_eq!(report.dream.by_verdict.obsolete, 1);
        assert_eq!(report.dream.by_verdict.superseded, 0);
        assert_eq!(
            report.dream.demoted_symbols, 0,
            "forgotten-symbol count is Full-channel only and must stay 0 under the default"
        );
        // Daemon/ledger bookkeeping stays visible in every mode.
        assert_eq!(
            report.dream.witnesses_ledgered, 1,
            "raw ledger stamp count is not verdict content and must stay visible"
        );
        assert!(report.dream.last_run.is_some());

        // Explicit opt-out suppresses the verdict-derived counts entirely.
        std::env::set_var("CSR_DREAM_CONSUMPTION", "0");
        let report_off = gather_status(&db_path, &projects_dir, false).unwrap();
        std::env::remove_var("CSR_DREAM_CONSUMPTION");
        assert_eq!(
            report_off.dream.events_total, 0,
            "verdict totals must be suppressed when consumption is Off"
        );
        assert_eq!(report_off.dream.by_verdict.obsolete, 0);
        assert_eq!(
            report_off.dream.witnesses_ledgered, 1,
            "ledger bookkeeping stays visible even when Off"
        );
    }

    #[test]
    fn status_dream_block_last_daemon_run_and_next_due_default_to_none() {
        let _guard = crate::daemon::dream_cadence::env_test_guard();
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let projects_dir = dir.path().join("projects");
        std::fs::create_dir_all(&projects_dir).unwrap();

        let _storage = Storage::open(&db_path).unwrap();
        let report = gather_status(&db_path, &projects_dir, false).unwrap();
        assert!(
            report.dream.last_daemon_run.is_none(),
            "daemon dream loop has never completed a cycle on a fresh DB"
        );
        assert!(
            report.dream.next_due.is_none(),
            "next_due depends on last_daemon_run — must also be None"
        );
    }

    #[test]
    fn status_dream_block_surfaces_daemon_cadence_once_persisted() {
        // `dream_cadence::env_test_guard` — shared with that module's own
        // env-var tests — because `gather_dream` reads `interval_secs()`,
        // which is process-global env state (see that guard's doc).
        let _guard = crate::daemon::dream_cadence::env_test_guard();
        std::env::set_var("CSR_DREAM_INTERVAL_SECS", "3600");

        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let projects_dir = dir.path().join("projects");
        std::fs::create_dir_all(&projects_dir).unwrap();

        let storage = Storage::open(&db_path).unwrap();
        let last = chrono::Utc::now() - chrono::Duration::hours(1);
        storage
            .set_meta(
                crate::daemon::dream_cadence::META_LAST_RUN_AT,
                &last.to_rfc3339(),
            )
            .unwrap();
        drop(storage);

        let report = gather_status(&db_path, &projects_dir, false).unwrap();
        std::env::remove_var("CSR_DREAM_INTERVAL_SECS");

        assert_eq!(
            report.dream.last_daemon_run.as_deref(),
            Some(last.to_rfc3339().as_str())
        );
        let expected_next_due = last + chrono::Duration::seconds(3600);
        assert_eq!(
            report.dream.next_due.as_deref(),
            Some(expected_next_due.to_rfc3339().as_str())
        );
    }

    #[test]
    fn status_dream_block_hides_next_due_when_daemon_dreaming_is_disabled() {
        let _guard = crate::daemon::dream_cadence::env_test_guard();
        std::env::set_var("CSR_NO_DREAMING", "1");
        let storage = Storage::open_memory().unwrap();
        storage
            .set_meta(
                crate::daemon::dream_cadence::META_LAST_RUN_AT,
                &chrono::Utc::now().to_rfc3339(),
            )
            .unwrap();
        let dream = gather_dream(&storage);
        std::env::remove_var("CSR_NO_DREAMING");
        assert!(!dream.daemon_enabled);
        assert!(dream.next_due.is_none());
        assert!(dream.last_daemon_run.is_some());
    }

    #[test]
    fn gather_dream_hides_verdict_counts_when_consumption_is_off() {
        // This test does NOT set CSR_DREAM_CONSUMPTION (matching the real
        // default), so `gather_dream`'s verdict-derived fields must all read
        // zero. This is a light smoke test; the "never queries witness_verdicts"
        // proof lives in mcp::tools and storage::recap_feeds.
        let storage = Storage::open_memory().unwrap();
        let dream = gather_dream(&storage);
        assert_eq!(dream.events_total, 0);
        assert_eq!(dream.by_verdict.obsolete, 0);
        assert_eq!(dream.by_verdict.superseded, 0);
        assert_eq!(dream.by_verdict.reinstated, 0);
        assert_eq!(dream.demoted_symbols, 0);
    }
}
