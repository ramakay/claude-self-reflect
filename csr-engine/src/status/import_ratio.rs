//! `imported_files / total_jsonl_files`: both sides over one file set.
//!
//! The denominator is the top-level transcripts on disk (`<projects>/<project>/*.jsonl`). The
//! numerator used to be every `import_state` row: subagent transcripts, plans, Codex rollouts
//! and files Claude Code has since deleted, so it read "33710/521 files" and a `.min(100.0)`
//! clamp hid the mismatch. Now the numerator is the transcripts in that same on-disk set that
//! have an `import_state` row, so it can never pass the denominator, and the whole table's row
//! count is reported apart as `import_state_rows`.

use std::path::{Path, PathBuf};

use crate::storage::Storage;

/// The top-level transcripts the import ratio counts.
pub(super) fn transcripts_on_disk(projects_dir: &Path) -> Vec<PathBuf> {
    crate::import::discover_projects(projects_dir)
        .unwrap_or_default()
        .into_iter()
        .flat_map(|(dir, _)| crate::import::list_jsonl_files(&dir).unwrap_or_default())
        .collect()
}

/// How many of `transcripts` have an `import_state` row (one primary-key lookup each).
pub(super) fn imported_of(storage: &Storage, transcripts: &[PathBuf]) -> usize {
    storage
        .with_connection(|conn| {
            let mut row = conn.prepare("SELECT 1 FROM import_state WHERE file_path = ?1")?;
            let mut imported = 0usize;
            for path in transcripts {
                if row.exists([path.to_string_lossy().as_ref()])? {
                    imported += 1;
                }
            }
            Ok(imported)
        })
        .unwrap_or(0)
}

/// Percent of `total` imported. No clamp: `imported` counts a subset of the same files.
pub(super) fn percent(imported: usize, total: usize) -> f64 {
    if total == 0 {
        return 0.0;
    }
    imported as f64 / total as f64 * 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_outside_the_on_disk_transcript_set_do_not_count() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("t.db")).unwrap();
        let projects = dir.path().join("projects");
        let project = projects.join("-Users-me-proj");
        std::fs::create_dir_all(project.join("s1/subagents")).unwrap();
        let a = project.join("a.jsonl");
        let b = project.join("b.jsonl");
        std::fs::write(&a, "{}\n").unwrap();
        std::fs::write(&b, "{}\n").unwrap();
        let rows = [
            a.to_string_lossy().to_string(),
            // a subagent transcript, a plan and a deleted transcript: not in the set
            project
                .join("s1/subagents/agent-1.jsonl")
                .to_string_lossy()
                .to_string(),
            "plan:my-plan".to_string(),
            project.join("gone.jsonl").to_string_lossy().to_string(),
        ];
        storage
            .with_connection(|conn| {
                for row in &rows {
                    conn.execute(
                        "INSERT INTO import_state (file_path, conversation_id, chunks_imported) VALUES (?1, 'c', 1)",
                        [row],
                    )?;
                }
                Ok(())
            })
            .unwrap();

        let on_disk = transcripts_on_disk(&projects);
        assert_eq!(on_disk.len(), 2);
        let imported = imported_of(&storage, &on_disk);
        assert_eq!(imported, 1, "four rows, one of them a transcript on disk");
        assert_eq!(percent(imported, on_disk.len()), 50.0);
        assert_eq!(percent(0, 0), 0.0);
    }
}
