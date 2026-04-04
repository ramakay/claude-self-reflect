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
pub fn handle(db_path: &Path, projects_dir: &Path, compact: bool) -> Result<()> {
    let report = gather_status(db_path, projects_dir)?;

    if compact {
        print_compact(&report);
    } else {
        println!("{}", serde_json::to_string_pretty(&report)?);
    }

    Ok(())
}

/// Gather status data from SQLite and disk.
fn gather_status(db_path: &Path, projects_dir: &Path) -> Result<StatusReport> {
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
    let healthy = storage.integrity_check().unwrap_or(false);

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

        let report = gather_status(&db_path, &projects_dir).unwrap();
        assert_eq!(report.conversations, 0);
        assert_eq!(report.chunks, 0);
        assert!(report.healthy); // empty DB passes integrity check
        assert_eq!(report.import_percent, 0.0);
    }
}
