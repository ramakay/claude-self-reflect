//! `aux.unimported_transcripts`: the part of the aux coverage gap that is a real miss.
//!
//! `aux.coverage.gap` counts `history.jsonl` sessions with no indexed chunk. Most of that gap is
//! not a fault: Claude Code deletes old transcripts, and a session that holds no message (a
//! remote-control bridge, an aborted start) is imported to zero chunks. Measured on one machine
//! (2026-11-17): a gap of 388 sessions, 374 of them with no transcript left on disk and the
//! other 14 imported to zero chunks. So the gap alone would read as degraded forever.
//!
//! This count keeps only sessions whose transcript is still on disk, older than
//! [`SETTLE_MINUTES`] (the watcher imports within seconds; a newer file may simply be in
//! flight), and which the importer never recorded in `import_state` at all.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::storage::Storage;

/// A transcript younger than this is not counted: it may still be on its way in.
pub const SETTLE_MINUTES: u64 = 10;

/// Registry sessions with no indexed chunk and no `import_state` row by conversation id.
const GAP_NOT_SEEN: &str = "SELECT sr.session_id FROM session_registry sr
    WHERE NOT EXISTS (SELECT 1 FROM chunks c WHERE c.conversation_id = sr.session_id
                        AND c.conversation_id NOT LIKE 'plan:%')
      AND NOT EXISTS (SELECT 1 FROM import_state i WHERE i.conversation_id = sr.session_id)";

/// Top-level transcripts under `projects_dir` older than the settle time, by session id.
fn settled_transcripts(projects_dir: &Path, now: SystemTime) -> HashMap<String, PathBuf> {
    let settle = Duration::from_secs(SETTLE_MINUTES * 60);
    let mut out = HashMap::new();
    for (dir, _) in crate::import::discover_projects(projects_dir).unwrap_or_default() {
        for path in crate::import::list_jsonl_files(&dir).unwrap_or_default() {
            let settled = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .is_ok_and(|modified| now.duration_since(modified).unwrap_or_default() >= settle);
            let stem = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned);
            if let (true, Some(stem)) = (settled, stem) {
                out.insert(stem, path);
            }
        }
    }
    out
}

/// How many registry sessions have a settled transcript on disk that the importer never saw.
/// `None` when the database cannot answer (a pre-registry schema).
pub fn count(storage: &Storage, projects_dir: &Path, now: SystemTime) -> Option<i64> {
    let on_disk = settled_transcripts(projects_dir, now);
    storage
        .with_connection(|conn| {
            let mut gap = conn.prepare(GAP_NOT_SEEN)?;
            let ids: Vec<String> = gap
                .query_map([], |row| row.get(0))?
                .collect::<Result<_, _>>()?;
            let mut by_path = conn.prepare("SELECT 1 FROM import_state WHERE file_path = ?1")?;
            let mut missed = 0i64;
            for id in ids {
                if let Some(path) = on_disk.get(&id) {
                    if !by_path.exists([path.to_string_lossy().as_ref()])? {
                        missed += 1;
                    }
                }
            }
            Ok(missed)
        })
        .ok()
}

/// `aux.transcripts_unindexed`: `~/.claude/transcripts/ses_*.jsonl` files whose session is in
/// neither the registry nor the chunks (moved here from `status.rs`).
pub fn count_unindexed_transcripts(storage: &Storage, claude_dir: Option<&Path>) -> usize {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn transcript(projects: &Path, session: &str) -> PathBuf {
        let dir = projects.join("-Users-me-proj");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{session}.jsonl"));
        std::fs::write(&path, "{}\n").unwrap();
        path
    }

    fn register(storage: &Storage, session: &str) {
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO session_registry (session_id, project, first_prompt, first_ts, last_ts, prompt_count)
                     VALUES (?1, 'proj', 'p', '2026-11-17T00:00:00Z', '2026-11-17T00:00:00Z', 1)",
                    [session],
                )?;
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn only_a_settled_transcript_the_importer_never_saw_counts() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("t.db")).unwrap();
        let projects = dir.path().join("projects");
        let later = SystemTime::now() + Duration::from_secs(3600);

        // deleted transcript: in the registry, gone from disk
        register(&storage, "gone");
        // imported to zero chunks: the importer saw it
        register(&storage, "empty");
        let empty = transcript(&projects, "empty");
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO import_state (file_path, conversation_id, chunks_imported) VALUES (?1, 'empty', 0)",
                    [empty.to_string_lossy().as_ref()],
                )?;
                Ok(())
            })
            .unwrap();
        // the real miss
        register(&storage, "missed");
        transcript(&projects, "missed");

        assert_eq!(count(&storage, &projects, later), Some(1));
        // the same miss, written just now, is still in flight
        assert_eq!(count(&storage, &projects, SystemTime::now()), Some(0));
    }
}
