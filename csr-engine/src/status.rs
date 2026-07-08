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
    pub enrichment: EnrichmentBreakdown,
    pub newest_chunk: Option<String>,
    pub db_size_bytes: u64,
    pub db_path: String,
    pub healthy: bool,
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
            conversations: 0,
            projects: 0,
            chunks: 0,
            reflections: 0,
            imported_files: 0,
            total_jsonl_files: total_jsonl,
            import_percent: 0.0,
            enrichment: EnrichmentBreakdown::default(),
            newest_chunk: None,
            db_size_bytes: 0,
            db_path: db_path.to_string_lossy().to_string(),
            healthy: false,
        });
    }

    let storage = Storage::open(db_path)?;

    let conversations = storage.count_conversations().unwrap_or(0);
    let projects = storage.count_projects().unwrap_or(0);
    let chunks = storage.count_chunk_embeddings().unwrap_or(0);
    let reflections = storage.count_reflection_embeddings().unwrap_or(0);
    let imported_files = storage.count_imported_files().unwrap_or(0);
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

    Ok(StatusReport {
        conversations,
        projects,
        chunks,
        reflections,
        imported_files,
        total_jsonl_files: total_jsonl,
        import_percent,
        enrichment,
        newest_chunk,
        db_size_bytes,
        db_path: db_path.to_string_lossy().to_string(),
        healthy,
    })
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
    // Format: [████████░░ 82%] [✓ 909c 54r] [3 projects]
    let bar_filled = (report.import_percent / 10.0).round() as usize;
    let bar_empty = 10_usize.saturating_sub(bar_filled);
    let bar: String = "█".repeat(bar_filled) + &"░".repeat(bar_empty);

    let health = if report.healthy { "ok" } else { "!!" };

    print!(
        "[{} {:.0}%] [{}] {}c {}r | {}p",
        bar,
        report.import_percent,
        health,
        report.conversations,
        report.reflections,
        report.projects,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_nonexistent_db() {
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

    #[test]
    fn test_compact_format() {
        let report = StatusReport {
            conversations: 909,
            projects: 3,
            chunks: 5000,
            reflections: 54,
            imported_files: 909,
            total_jsonl_files: 1000,
            import_percent: 90.9,
            enrichment: EnrichmentBreakdown::default(),
            newest_chunk: None,
            db_size_bytes: 100_000_000,
            db_path: "/tmp/test.db".to_string(),
            healthy: true,
        };
        // Just verify it doesn't panic
        print_compact(&report);
    }

    #[test]
    fn test_status_with_empty_db() {
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
}
