//! Batched, resumable backfill: records each already-imported conversation's
//! transcript `"entrypoint"` (`cli`, `sdk-cli`, `sdk-*`, `claude-desktop`,
//! ...) into `conversation_entrypoint`, for conversations imported before
//! this field was captured live (see
//! `import::ParsedConversation::entrypoint`, wired in `engine.rs` and
//! `import::watcher`).
//!
//! Without this, `storage::queries::filter_unregistered_conversation_rows`
//! reads every pre-existing conversation as "unknown" and fails it open
//! (shows it) — correct for a real session, but it also resurfaces headless
//! `sdk-cli` conversations that the (overbroad, now-removed)
//! `session_registry`-absence check used to hide. This backfill restores
//! that exclusion without re-embedding a single chunk: it only reads the
//! transcript's `entrypoint` field, never touches `chunks` or embeddings.
//!
//! Resumable via a `file_path` cursor in `meta` (`import_state` is walked in
//! `file_path` order): each call processes one bounded batch and advances
//! the cursor, so a multi-thousand-conversation backlog never holds the
//! daemon's heavy-work permit for one long synchronous pass. A file that no
//! longer exists, or never carried an `entrypoint` field within the scan
//! cap, is not retried on a later call — its status stays "unknown", which
//! fails open, exactly the same as if this backfill had never run at all.
//!
//! Idempotent: a `conversation_entrypoint` row, once written, drops the
//! conversation out of every future batch's `LEFT JOIN ... IS NULL` filter.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::Result;

use crate::storage::Storage;

/// Bounded scan depth per file — the harness stamps `entrypoint` on the
/// file's own early records (SessionStart hook, first user/assistant turn),
/// never only near the end, so a bounded prefix scan is enough without
/// reading a multi-MB transcript in full.
const SCAN_LINE_CAP: usize = 500;

/// Meta key: cursor past which `pending_entrypoint_backfill_batch` should
/// resume (empty string covers "from the start").
pub const CURSOR_META_KEY: &str = "conversation_entrypoint_backfill_cursor_v1";

/// Meta key: present once a batch has drained the whole backlog.
pub const DONE_META_KEY: &str = "conversation_entrypoint_backfill_done_v1";

/// Read the first `"entrypoint"` field value from `path`'s leading
/// `SCAN_LINE_CAP` lines, or `None` when the file is unreadable, has no such
/// field in that prefix, or the field is present but empty.
fn read_entrypoint(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    for line in reader.lines().take(SCAN_LINE_CAP) {
        let line = line.ok()?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parsed: serde_json::Value = match sonic_rs::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(ep) = parsed.get("entrypoint").and_then(|v| v.as_str()) {
            if !ep.is_empty() {
                return Some(ep.to_string());
            }
        }
    }
    None
}

/// Receipt for one [`backfill_conversation_entrypoints_batch`] call.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct EntrypointBackfillStats {
    /// `import_state` rows examined this call.
    pub scanned: usize,
    /// Of those, how many got a `conversation_entrypoint` row written.
    pub recorded: usize,
    /// Rows whose transcript is gone, or never carries the field within the
    /// scan cap — left unrecorded (fails open), not retried.
    pub missing_or_unknown: usize,
    /// True once this call found fewer than `batch_size` pending rows — the
    /// whole backlog existing at the time of the first call has been swept.
    pub done: bool,
}

/// Process one bounded batch of the backfill. Safe to call repeatedly
/// (e.g. on a daemon tick): each call advances (or completes) the cursor in
/// `meta`, and does nothing once [`DONE_META_KEY`] is set.
pub fn backfill_conversation_entrypoints_batch(
    storage: &Storage,
    batch_size: usize,
) -> Result<EntrypointBackfillStats> {
    if storage.get_meta(DONE_META_KEY)?.is_some() {
        return Ok(EntrypointBackfillStats {
            done: true,
            ..EntrypointBackfillStats::default()
        });
    }
    let cursor = storage.get_meta(CURSOR_META_KEY)?.unwrap_or_default();
    let pending = storage.pending_entrypoint_backfill_batch(&cursor, batch_size)?;
    let mut stats = EntrypointBackfillStats::default();
    let mut last_path = cursor;
    for (file_path, conversation_id) in &pending {
        stats.scanned += 1;
        last_path = file_path.clone();
        match read_entrypoint(Path::new(file_path)) {
            Some(ep) => {
                storage.set_conversation_entrypoint(conversation_id, &ep)?;
                stats.recorded += 1;
            }
            None => stats.missing_or_unknown += 1,
        }
    }
    if pending.len() < batch_size {
        stats.done = true;
        storage.set_meta(DONE_META_KEY, "1")?;
    } else {
        storage.set_meta(CURSOR_META_KEY, &last_path)?;
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes a minimal transcript file and registers it in `import_state`
    /// via the same public `mark_file_imported` path a real import uses —
    /// `conversation_id` is derived from the file stem, exactly as
    /// `mark_file_imported`'s own doc describes.
    fn write_and_register(
        storage: &Storage,
        dir: &std::path::Path,
        conv_id: &str,
        entrypoint: Option<&str>,
    ) -> std::path::PathBuf {
        let path = dir.join(format!("{conv_id}.jsonl"));
        let body = match entrypoint {
            Some(ep) => format!(
                "{{\"type\":\"user\",\"entrypoint\":\"{ep}\",\"message\":{{\"role\":\"user\",\"content\":\"hi\"}}}}\n"
            ),
            None => "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n"
                .to_string(),
        };
        std::fs::write(&path, body).unwrap();
        storage.mark_file_imported(&path, 1).unwrap();
        path
    }

    #[test]
    fn backfill_records_entrypoint_from_transcript_and_completes() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Storage::open_memory().unwrap();
        write_and_register(&storage, tmp.path(), "conv-cli", Some("cli"));
        write_and_register(&storage, tmp.path(), "conv-sdk", Some("sdk-cli"));
        write_and_register(&storage, tmp.path(), "conv-unknown", None);

        let stats = backfill_conversation_entrypoints_batch(&storage, 10).unwrap();
        assert_eq!(stats.scanned, 3);
        assert_eq!(stats.recorded, 2);
        assert_eq!(stats.missing_or_unknown, 1);
        assert!(
            stats.done,
            "a batch smaller than batch_size must complete: {stats:?}"
        );
        assert_eq!(
            storage.get_conversation_entrypoint("conv-cli").unwrap(),
            Some("cli".to_string())
        );
        assert_eq!(
            storage.get_conversation_entrypoint("conv-sdk").unwrap(),
            Some("sdk-cli".to_string())
        );
        assert_eq!(
            storage.get_conversation_entrypoint("conv-unknown").unwrap(),
            None,
            "no entrypoint field in the transcript must stay unrecorded (fails open), not guessed"
        );

        // Idempotent: a second call is a no-op (meta-gated), not a rescan.
        let second = backfill_conversation_entrypoints_batch(&storage, 10).unwrap();
        assert_eq!(second.scanned, 0);
        assert!(second.done);
    }

    #[test]
    fn backfill_resumes_across_batches_via_cursor() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Storage::open_memory().unwrap();
        for i in 0..5 {
            write_and_register(&storage, tmp.path(), &format!("conv-{i}"), Some("cli"));
        }

        let first = backfill_conversation_entrypoints_batch(&storage, 2).unwrap();
        assert_eq!(first.scanned, 2);
        assert!(!first.done, "a full batch must not claim completion yet");

        let second = backfill_conversation_entrypoints_batch(&storage, 2).unwrap();
        assert_eq!(second.scanned, 2);
        assert!(!second.done);

        let third = backfill_conversation_entrypoints_batch(&storage, 2).unwrap();
        assert_eq!(third.scanned, 1);
        assert!(third.done);

        for i in 0..5 {
            assert_eq!(
                storage
                    .get_conversation_entrypoint(&format!("conv-{i}"))
                    .unwrap(),
                Some("cli".to_string()),
                "conv-{i} must be recorded by the time the backlog is drained"
            );
        }
    }
}
