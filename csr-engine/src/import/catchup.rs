//! Idle-gated reconciliation sweep between `import_state.file_mtime` and the
//! live filesystem.
//!
//! The watcher's busy-retry backoff (`import::watcher::process_batch`)
//! recovers a file whose import failed with `database is locked` *as long as
//! another notify event eventually arrives*. When the failure lands on a
//! file's very last write — a sidechain's final append at session end, most
//! often — nothing ever touches that file again, so no event follows the
//! retry schedule's give-up point. This sweep is the backstop: it walks the
//! real corpus, compares each conversation file's on-disk mtime against what
//! `import_state` recorded, and re-sends anything stale through the same
//! import path the watcher uses. Run once at daemon startup and every 30
//! minutes after (see `daemon::mod::catchup_loop`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Utc};

use crate::import::{self, TranscriptKind};
use crate::storage::Storage;

/// How far back a stale file must have been touched to qualify. Bounds the
/// per-run scan cost on a multi-year corpus: a file whose last write predates
/// this was not affected by a *recent* busy failure, so leaving it alone does
/// not reopen the gap this sweep exists to close.
pub const CATCHUP_MAX_AGE_DAYS: i64 = 14;

/// Meta key `catchup_loop` persists the latest run's counts under.
pub const CATCHUP_LAST_META_KEY: &str = "import_catchup_last";
/// Meta counter key for cumulative files actually re-imported by the sweep.
pub const CATCHUP_REIMPORTED_COUNTER: &str = "import_catchup_reimported";

/// Sentinel error message the daemon's per-file import closure returns when
/// it declines a stale file because user activity resumed mid-sweep (the
/// idle-gate re-check after acquiring the heavy-work permit, or before
/// starting the next file). `catchup_sweep` recognizes this exact string and
/// counts it as `deferred` rather than `failed` — the file's `import_state`
/// row is untouched, so it stays stale and the next sweep picks it up.
pub(crate) const DEFERRED_NOT_IDLE: &str = "csr_catchup_deferred_not_idle";

/// Counts from one sweep pass.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CatchupSummary {
    /// Conversation JSONL files examined under `projects_dir`.
    pub scanned: usize,
    /// Of those, files whose fs mtime is newer than the recorded
    /// `import_state.file_mtime` (or have no row at all) and within
    /// [`CATCHUP_MAX_AGE_DAYS`].
    pub stale: usize,
    /// Stale files actually sent through `import` and returning `Ok`.
    pub reimported: usize,
    /// Stale files pre-filtered as CSR's own narrator/agent transcripts
    /// (`import::classify_transcript`) — never sent through `import`.
    pub skipped: usize,
    /// Stale files sent through `import` that returned `Err`.
    pub failed: usize,
    /// Stale files whose import was declined because user activity resumed
    /// mid-sweep (see [`DEFERRED_NOT_IDLE`]) — left stale for the next pass,
    /// not counted as a failure.
    pub deferred: usize,
}

impl CatchupSummary {
    /// JSON payload persisted to `meta[CATCHUP_LAST_META_KEY]`.
    pub fn to_meta_json(self, at: DateTime<Utc>) -> String {
        serde_json::json!({
            "scanned": self.scanned,
            "stale": self.stale,
            "reimported": self.reimported,
            "skipped": self.skipped,
            "failed": self.failed,
            "deferred": self.deferred,
            "at": at.to_rfc3339(),
        })
        .to_string()
    }
}

/// True when `fs_mtime` is newer than the recorded `stored_mtime`
/// (`import_state.file_mtime`'s RFC3339 format — see
/// `storage::queries::mark_file_imported`'s `file_mtime_str`), or there is no
/// recorded mtime at all. A stored value that fails to parse (never written
/// by this codebase, but a defensive floor) is treated as stale rather than
/// silently never reconciled.
pub(crate) fn is_stale(stored_mtime: Option<&str>, fs_mtime: DateTime<Utc>) -> bool {
    match stored_mtime {
        None => true,
        Some(stored) => match DateTime::parse_from_rfc3339(stored) {
            Ok(stored_dt) => fs_mtime > stored_dt.with_timezone(&Utc),
            Err(_) => true,
        },
    }
}

/// One pass of the catch-up sweep. Walks every project directory under
/// `projects_dir` (`import::discover_projects` +
/// `import::list_conversation_jsonl_files`, the same recursive walk the
/// initial full import uses, so sidechains under `<session>/subagents/` are
/// covered too), diffs each file's fs mtime against `import_state`, and sends
/// every stale, non-narrator candidate through `import`.
///
/// `import` mirrors the watcher's single-file import path — the production
/// caller passes a closure that calls `Engine::import_file` under a
/// per-file `heavy_work` permit (see `daemon::mod::catchup_loop`) — so this
/// function itself never touches embeddings/search and stays hermetically
/// testable over a bare `Storage` + tempdir.
pub async fn catchup_sweep<F, Fut>(
    storage: &Storage,
    projects_dir: &Path,
    now: DateTime<Utc>,
    mut import: F,
) -> Result<CatchupSummary>
where
    F: FnMut(PathBuf, String) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let mut summary = CatchupSummary::default();
    let known: HashMap<String, String> = storage.list_import_state_entries()?.into_iter().collect();

    let projects = import::discover_projects(projects_dir)?;
    for (dir, project_name) in projects {
        let files = import::list_conversation_jsonl_files(&dir)?;
        for file_path in files {
            summary.scanned += 1;

            let Ok(metadata) = std::fs::metadata(&file_path) else {
                continue;
            };
            let Ok(modified) = metadata.modified() else {
                continue;
            };
            let fs_mtime: DateTime<Utc> = modified.into();
            if now.signed_duration_since(fs_mtime) > chrono::Duration::days(CATCHUP_MAX_AGE_DAYS) {
                continue;
            }

            let path_str = file_path.to_string_lossy().to_string();
            if !is_stale(known.get(&path_str).map(String::as_str), fs_mtime) {
                continue;
            }
            summary.stale += 1;

            if import::classify_transcript(&file_path) == TranscriptKind::Agent {
                summary.skipped += 1;
                continue;
            }

            match import(file_path.clone(), project_name.clone()).await {
                Ok(()) => summary.reimported += 1,
                Err(error) if error.to_string() == DEFERRED_NOT_IDLE => {
                    summary.deferred += 1;
                    tracing::debug!(
                        file = %file_path.display(),
                        "catch-up sweep deferring import: activity resumed"
                    );
                }
                Err(error) => {
                    summary.failed += 1;
                    tracing::warn!(
                        file = %file_path.display(),
                        error = %error,
                        "catch-up sweep import failed"
                    );
                }
            }
        }
    }

    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use tokio::sync::RwLock;

    use crate::embeddings::EmbeddingEngine;
    use crate::search::SearchEngine;

    fn write_transcript(path: &Path, content: &str) {
        let line = serde_json::json!({
            "uuid": "u1",
            "type": "user",
            "message": {"role": "user", "content": content},
        });
        std::fs::write(path, line.to_string()).unwrap();
    }

    fn fs_mtime_rfc3339(path: &Path) -> String {
        let modified = std::fs::metadata(path).unwrap().modified().unwrap();
        let dt: DateTime<Utc> = modified.into();
        dt.to_rfc3339()
    }

    #[test]
    fn no_recorded_mtime_is_stale() {
        assert!(is_stale(None, Utc::now()));
    }

    #[test]
    fn newer_fs_mtime_is_stale() {
        let stored = "2020-01-01T00:00:00Z";
        assert!(is_stale(Some(stored), Utc::now()));
    }

    #[test]
    fn matching_fs_mtime_is_not_stale() {
        let now = Utc::now();
        let stored = now.to_rfc3339();
        assert!(!is_stale(Some(&stored), now));
    }

    #[test]
    fn malformed_stored_value_is_treated_as_stale() {
        assert!(is_stale(Some("not-a-timestamp"), Utc::now()));
    }

    /// Tempdir projects dir + a stale row (fs mtime newer than recorded
    /// `file_mtime`) and a current row (recorded `file_mtime` equal to the
    /// real fs mtime). The sweep must reimport only the stale file and align
    /// its `file_mtime`, leaving the current file's row untouched.
    ///
    /// Reverting `is_stale` to always return `false` (or deleting the sweep's
    /// call into `import`) makes this fail: `imported` stays empty and the
    /// stale row's `file_mtime` never moves off the seeded `2020-01-01` value.
    #[tokio::test]
    async fn sweep_reimports_only_the_stale_row() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = dir.path().join("-Users-test-proj");
        std::fs::create_dir_all(&project_dir).unwrap();

        let stale_path = project_dir.join("stale-conv.jsonl");
        let current_path = project_dir.join("current-conv.jsonl");
        write_transcript(&stale_path, "please fix the flaky test");
        write_transcript(&current_path, "please fix the other flaky test");

        let storage = Storage::open_memory().unwrap();
        storage
            .upsert_import_state_explicit(
                &stale_path.to_string_lossy(),
                "stale-conv",
                1,
                "2020-01-01T00:00:00Z",
            )
            .unwrap();
        let current_mtime = fs_mtime_rfc3339(&current_path);
        storage
            .upsert_import_state_explicit(
                &current_path.to_string_lossy(),
                "current-conv",
                1,
                &current_mtime,
            )
            .unwrap();

        let imported = Arc::new(Mutex::new(Vec::new()));
        let imported_for_closure = imported.clone();
        let summary = catchup_sweep(&storage, dir.path(), Utc::now(), move |path, _project| {
            let imported = imported_for_closure.clone();
            let storage_path = path.clone();
            async move {
                imported.lock().unwrap().push(storage_path.clone());
                Ok(())
            }
        })
        .await
        .unwrap();

        assert_eq!(summary.scanned, 2);
        assert_eq!(summary.stale, 1);
        assert_eq!(summary.reimported, 1);
        assert_eq!(summary.failed, 0);
        assert_eq!(
            imported.lock().unwrap().as_slice(),
            std::slice::from_ref(&stale_path)
        );
    }

    /// A narrator-shaped transcript (opening prompt matches
    /// `AGENT_PROMPT_SIGNATURES`, the exact predicate `import::classify_transcript`
    /// shares with the live import path) with a stale row must be counted as
    /// `skipped`, not sent through `import` at all — deleting the
    /// `classify_transcript` guard makes this fail because `import` gets
    /// called for it.
    #[tokio::test]
    async fn narrator_transcript_is_skipped_not_imported() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = dir.path().join("-Users-test-proj");
        std::fs::create_dir_all(&project_dir).unwrap();

        let narrator_path = project_dir.join("narrator-conv.jsonl");
        write_transcript(
            &narrator_path,
            "[[CSR:RECAP]] machine-marker: this is a CSR narrator side-session.\nYou are a post-turn narrator.",
        );

        let storage = Storage::open_memory().unwrap();
        storage
            .upsert_import_state_explicit(
                &narrator_path.to_string_lossy(),
                "narrator-conv",
                1,
                "2020-01-01T00:00:00Z",
            )
            .unwrap();

        let imported = Arc::new(Mutex::new(Vec::new()));
        let imported_for_closure = imported.clone();
        let summary = catchup_sweep(&storage, dir.path(), Utc::now(), move |path, _project| {
            let imported = imported_for_closure.clone();
            async move {
                imported.lock().unwrap().push(path);
                Ok(())
            }
        })
        .await
        .unwrap();

        assert_eq!(summary.stale, 1);
        assert_eq!(summary.skipped, 1);
        assert_eq!(summary.reimported, 0);
        assert!(imported.lock().unwrap().is_empty());
    }

    /// Files older than `CATCHUP_MAX_AGE_DAYS` are out of scope even with no
    /// `import_state` row at all — the sweep must not treat the whole corpus
    /// as perpetually stale.
    #[tokio::test]
    async fn ancient_untouched_file_is_out_of_scope() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = dir.path().join("-Users-test-proj");
        std::fs::create_dir_all(&project_dir).unwrap();
        let old_path = project_dir.join("ancient.jsonl");
        write_transcript(&old_path, "an old session");

        let storage = Storage::open_memory().unwrap();
        let far_future = Utc::now() + chrono::Duration::days(CATCHUP_MAX_AGE_DAYS + 30);

        let imported = Arc::new(Mutex::new(Vec::new()));
        let imported_for_closure = imported.clone();
        let summary = catchup_sweep(&storage, dir.path(), far_future, move |path, _project| {
            let imported = imported_for_closure.clone();
            async move {
                imported.lock().unwrap().push(path);
                Ok(())
            }
        })
        .await
        .unwrap();

        assert_eq!(summary.stale, 0);
        assert!(imported.lock().unwrap().is_empty());
    }

    /// Opus review, high + low findings on gap-G1: the sweep must run stale
    /// candidates through the exact same content-diffing import the live
    /// watcher uses (`import::watcher::import_file_content_diff`), not a
    /// mock and not `Engine::import_file`'s count-incremental path. That
    /// count-incremental path treats "parsed chunk count <= previously
    /// recorded count" as nothing new and just rewrites `file_mtime` — so a
    /// busy-dropped tail that only grows the transcript's *existing* final
    /// chunk (the common case: a session's last append before the process
    /// that held the lock exits) gets permanently stamped as reimported
    /// without the grown content ever being re-embedded, and no later sweep
    /// would see the file as stale again to retry it.
    ///
    /// This test imports a file for real, appends a short line that grows
    /// the transcript's one existing chunk (no new chunk boundary), forces
    /// the recorded `file_mtime` back to stale — the exact state a
    /// busy-dropped write leaves behind — runs the sweep through the real
    /// production import fn, and asserts both that `file_mtime` catches up
    /// AND that the stored chunk content actually contains the appended
    /// text. A second, untouched file's row and content must be unaffected.
    ///
    /// Swapping the closure's import call for
    /// `Engine::import_file_with_attribution`'s count-incremental path
    /// reproduces the exact bug this test closes: `file_mtime` still catches
    /// up (so a `file_mtime`-only assertion, like the mock-based
    /// `sweep_reimports_only_the_stale_row` test above, would keep passing)
    /// but the stored chunk content stays the pre-append text — the content
    /// assertion below is what catches it.
    #[tokio::test]
    async fn sweep_reimports_via_real_production_import_and_grows_final_chunk() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = dir.path().join("-Users-test-proj");
        std::fs::create_dir_all(&project_dir).unwrap();

        let stale_path = project_dir.join("stale-conv.jsonl");
        let current_path = project_dir.join("current-conv.jsonl");
        write_transcript(&stale_path, "please fix the flaky test");
        write_transcript(&current_path, "please fix the other flaky test");

        let storage = Storage::open_memory().unwrap();
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let search = Arc::new(RwLock::new(SearchEngine::new(16)));

        // Real first import of both files — exactly what the live watcher
        // (or the initial full import) would have done originally.
        crate::import::watcher::import_file_content_diff(
            &storage,
            &embeddings,
            &search,
            dir.path(),
            &stale_path,
        )
        .await
        .unwrap();
        crate::import::watcher::import_file_content_diff(
            &storage,
            &embeddings,
            &search,
            dir.path(),
            &current_path,
        )
        .await
        .unwrap();

        let chunk_contents = |storage: &Storage, conv_id: &str| -> Vec<String> {
            storage
                .with_connection(|conn| {
                    let mut stmt =
                        conn.prepare("SELECT content FROM chunks WHERE conversation_id = ?1")?;
                    let rows = stmt
                        .query_map([conv_id], |row| row.get::<_, String>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    Ok(rows)
                })
                .unwrap()
        };

        let current_mtime_before = fs_mtime_rfc3339(&current_path);
        let current_content_before = chunk_contents(&storage, "current-conv");
        assert!(!current_content_before.is_empty());

        // Append a short line to the stale file's tail. The parser folds a
        // short single-user-message transcript into one chunk, so this grows
        // that existing chunk rather than opening a new one — the same
        // shape as a live session's last, busy-dropped append.
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&stale_path)
                .unwrap();
            writeln!(f).unwrap();
            writeln!(
                f,
                "{}",
                serde_json::json!({
                    "uuid": "u2",
                    "type": "user",
                    "message": {"role": "user", "content": "a zebra crossed the flaky test"},
                })
            )
            .unwrap();
        }

        // Simulate the busy-drop: `import_state.file_mtime` for the stale
        // file still points at the pre-append value even though the file on
        // disk has moved on.
        let prev_chunk_count = storage.get_imported_chunk_count(&stale_path).unwrap();
        storage
            .upsert_import_state_explicit(
                &stale_path.to_string_lossy(),
                "stale-conv",
                prev_chunk_count,
                "2020-01-01T00:00:00Z",
            )
            .unwrap();

        let summary = catchup_sweep(&storage, dir.path(), Utc::now(), |path, _project| {
            let embeddings = embeddings.clone();
            let search = search.clone();
            let projects_dir = dir.path().to_path_buf();
            let storage_ref = &storage;
            async move {
                crate::import::watcher::import_file_content_diff(
                    storage_ref,
                    &embeddings,
                    &search,
                    &projects_dir,
                    &path,
                )
                .await
            }
        })
        .await
        .unwrap();

        assert_eq!(summary.reimported, 1, "summary: {summary:?}");
        assert_eq!(summary.failed, 0, "summary: {summary:?}");
        assert_eq!(summary.deferred, 0, "summary: {summary:?}");

        let stale_mtime_after = storage
            .list_import_state_entries()
            .unwrap()
            .into_iter()
            .find(|(path, _)| *path == stale_path.to_string_lossy())
            .map(|(_, mtime)| mtime);
        assert_eq!(
            stale_mtime_after.as_deref(),
            Some(fs_mtime_rfc3339(&stale_path).as_str()),
            "file_mtime must catch up to the post-append fs mtime"
        );

        let stale_content_after = chunk_contents(&storage, "stale-conv");
        assert!(
            stale_content_after.iter().any(|c| c.contains("zebra")),
            "appended tail must be re-embedded and stored, not silently dropped: {stale_content_after:?}"
        );

        // The untouched file must be unaffected by the sweep.
        let current_mtime_after = storage
            .list_import_state_entries()
            .unwrap()
            .into_iter()
            .find(|(path, _)| *path == current_path.to_string_lossy())
            .map(|(_, mtime)| mtime);
        assert_eq!(
            current_mtime_after.as_deref(),
            Some(current_mtime_before.as_str())
        );
        assert_eq!(
            chunk_contents(&storage, "current-conv"),
            current_content_before
        );
    }

    /// Opus review, medium finding on gap-G1 (idle gating): the daemon's
    /// per-file closure returns the [`DEFERRED_NOT_IDLE`] sentinel when
    /// activity resumes mid-sweep. `catchup_sweep` must count that as
    /// `deferred`, not `failed` and not `reimported` — a deferred file's
    /// `import_state` row is untouched by design, so the next sweep pass
    /// picks it back up, unlike a real failure (which is also left stale
    /// but logged at `warn` instead of `debug`).
    #[tokio::test]
    async fn deferred_not_idle_is_counted_separately_from_failed() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = dir.path().join("-Users-test-proj");
        std::fs::create_dir_all(&project_dir).unwrap();
        let stale_path = project_dir.join("stale-conv.jsonl");
        write_transcript(&stale_path, "please fix the flaky test");

        let storage = Storage::open_memory().unwrap();

        let summary = catchup_sweep(&storage, dir.path(), Utc::now(), |_path, _project| async {
            Err(anyhow::anyhow!(DEFERRED_NOT_IDLE))
        })
        .await
        .unwrap();

        assert_eq!(summary.stale, 1);
        assert_eq!(summary.deferred, 1);
        assert_eq!(summary.failed, 0);
        assert_eq!(summary.reimported, 0);
    }
}
