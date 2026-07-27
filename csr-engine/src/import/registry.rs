//! Incremental ingest of `~/.claude/history.jsonl` into `session_registry`.
//!
//! Data is NEVER embedded and NEVER injected into search — table + status only.
//! Call site for `ingest_history` lives in the daemon loop (another lane).

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use anyhow::Result;
use sha2::{Digest, Sha256};

use crate::storage::queries::{self, SessionRegistryRow};
use crate::storage::Storage;

const META_OFFSET: &str = "history_jsonl_offset";
const META_INODE: &str = "history_jsonl_inode";
const META_HEAD: &str = "history_jsonl_head";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IngestStats {
    pub lines_read: usize,
    pub sessions_upserted: usize,
    pub parse_errors: usize,
}

/// Incrementally ingest `~/.claude/history.jsonl` into `session_registry`.
///
/// Called ONLY from the daemon loop (single writer) — never from setup or
/// any other CLI path.
///
/// **Transactional checkpoint:** row upserts and the three meta checkpoint
/// keys (`history_jsonl_offset`, `history_jsonl_inode`, `history_jsonl_head`)
/// commit in one `Storage::with_transaction` call. A crash between "upserted
/// the rows" and "advanced the offset" is impossible — on restart the batch
/// is either fully applied or fully re-read, never half-committed.
pub fn ingest_history(storage: &Storage, history_path: &Path) -> Result<IngestStats> {
    if !history_path.exists() {
        return Ok(IngestStats::default());
    }

    let metadata = std::fs::metadata(history_path)?;
    let file_size = metadata.len();
    let inode = metadata.ino();

    // First line for head-hash (bounded read — do not load whole file).
    let first_line_bytes = read_first_line_bytes(history_path)?;
    let head_hash = hex_sha256(&first_line_bytes);

    // Load checkpoint meta (outside the write transaction).
    let stored_offset = storage
        .get_meta(META_OFFSET)?
        .and_then(|v| v.parse::<u64>().ok());
    let stored_inode = storage.get_meta(META_INODE)?;
    let stored_head = storage.get_meta(META_HEAD)?;

    let has_checkpoint = stored_offset.is_some() || stored_inode.is_some() || stored_head.is_some();

    let mut offset = stored_offset.unwrap_or(0);
    if has_checkpoint {
        let inode_ok = stored_inode.as_deref() == Some(&inode.to_string());
        let head_ok = stored_head.as_deref() == Some(head_hash.as_str());
        let size_ok = offset <= file_size;
        if !inode_ok || !head_ok || !size_ok {
            // File replaced or truncated — re-read from byte 0.
            offset = 0;
        }
    }

    // Read + parse without holding the DB lock.
    let (lines_read, sessions, parse_errors, new_offset) =
        read_parse_from_offset(history_path, offset)?;

    let rows: Vec<SessionRegistryRow> = sessions.into_values().collect();
    let sessions_upserted = rows.len();
    let stats = IngestStats {
        lines_read,
        sessions_upserted,
        parse_errors,
    };

    let inode_str = inode.to_string();
    let offset_str = new_offset.to_string();
    let head_hash_for_tx = head_hash.clone();

    // Atomic: upserts + checkpoint meta in one transaction.
    storage.with_transaction(|tx| {
        queries::upsert_session_registry_batch(tx, &rows)?;
        queries::set_meta(tx, META_OFFSET, &offset_str)?;
        queries::set_meta(tx, META_INODE, &inode_str)?;
        queries::set_meta(tx, META_HEAD, &head_hash_for_tx)?;
        Ok(())
    })?;

    // At most once per ingest_history call (outside the write txn — separate
    // meta keys, and bump_aux_counter takes its own lock).
    if parse_errors > 0 {
        let _ = storage.bump_aux_counter("history");
    }

    Ok(stats)
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn read_first_line_bytes(path: &Path) -> Result<Vec<u8>> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut buf = Vec::new();
    reader.read_until(b'\n', &mut buf)?;
    Ok(buf)
}

/// Read from `offset`, parse JSONL lines, aggregate per-session.
/// Returns (lines_read, session_map, parse_errors, new_byte_offset).
fn read_parse_from_offset(
    path: &Path,
    offset: u64,
) -> Result<(usize, HashMap<String, SessionRegistryRow>, usize, u64)> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::Start(offset))?;

    let mut sessions: HashMap<String, SessionRegistryRow> = HashMap::new();
    let mut lines_read = 0usize;
    let mut parse_errors = 0usize;
    let mut bytes_read = 0u64;
    let mut buf = Vec::new();

    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 {
            break;
        }
        bytes_read += n as u64;
        lines_read += 1;

        // Strip trailing newline for parsing.
        let line = trim_line_ending(&buf);
        if line.is_empty() {
            continue;
        }
        let line_str = match std::str::from_utf8(line) {
            Ok(s) => s,
            Err(_) => {
                parse_errors += 1;
                continue;
            }
        };

        match parse_history_line(line_str) {
            Some(row) => merge_session(&mut sessions, row),
            None => parse_errors += 1,
        }
    }

    Ok((lines_read, sessions, parse_errors, offset + bytes_read))
}

fn trim_line_ending(buf: &[u8]) -> &[u8] {
    let mut end = buf.len();
    if end > 0 && buf[end - 1] == b'\n' {
        end -= 1;
    }
    if end > 0 && buf[end - 1] == b'\r' {
        end -= 1;
    }
    &buf[..end]
}

fn parse_history_line(line: &str) -> Option<SessionRegistryRow> {
    // Match import/mod.rs: parse via sonic_rs into serde_json::Value for field access.
    let value: serde_json::Value = sonic_rs::from_str(line).ok()?;

    let display = value.get("display")?.as_str()?.to_string();
    let timestamp_ms = value
        .get("timestamp")?
        .as_u64()
        .or_else(|| value.get("timestamp")?.as_i64().map(|n| n as u64))?;
    let project_path = value.get("project")?.as_str()?;
    let session_id = value.get("sessionId")?.as_str()?.to_string();

    if session_id.is_empty() || project_path.is_empty() {
        return None;
    }

    let project = Path::new(project_path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(project_path)
        .to_string();

    let ts = chrono::DateTime::from_timestamp_millis(timestamp_ms as i64)
        .unwrap_or_default()
        .to_rfc3339();

    Some(SessionRegistryRow {
        session_id,
        project,
        first_prompt: Some(display),
        first_ts: ts.clone(),
        last_ts: ts,
        prompt_count_delta: 1,
    })
}

fn merge_session(map: &mut HashMap<String, SessionRegistryRow>, row: SessionRegistryRow) {
    map.entry(row.session_id.clone())
        .and_modify(|existing| {
            existing.prompt_count_delta += row.prompt_count_delta;
            if row.first_ts < existing.first_ts {
                existing.first_ts = row.first_ts.clone();
                existing.first_prompt = row.first_prompt.clone();
                existing.project = row.project.clone();
            }
            if row.last_ts > existing.last_ts {
                existing.last_ts = row.last_ts.clone();
            }
        })
        .or_insert(row);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::ConversationChunk;
    use crate::provenance::Speaker;
    use crate::storage::Storage;
    use std::io::Write;

    fn history_line(display: &str, ts_ms: u64, project: &str, session_id: &str) -> String {
        format!(
            r#"{{"display":"{display}","timestamp":{ts_ms},"project":"{project}","sessionId":"{session_id}"}}"#
        )
    }

    fn write_history(path: &Path, lines: &[&str]) {
        let mut f = File::create(path).unwrap();
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }
    }

    fn append_history(path: &Path, lines: &[&str]) {
        let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }
    }

    fn registry_row(
        storage: &Storage,
        session_id: &str,
    ) -> Option<(String, Option<String>, String, String, i64)> {
        storage
            .with_transaction(|tx| {
                let result = tx.query_row(
                    "SELECT project, first_prompt, first_ts, last_ts, prompt_count
                     FROM session_registry WHERE session_id = ?1",
                    [session_id],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, Option<String>>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, String>(3)?,
                            r.get::<_, i64>(4)?,
                        ))
                    },
                );
                match result {
                    Ok(row) => Ok(Some(row)),
                    Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                    Err(e) => Err(e.into()),
                }
            })
            .unwrap()
    }

    fn sum_prompt_count(storage: &Storage) -> i64 {
        storage
            .with_transaction(|tx| {
                tx.query_row(
                    "SELECT COALESCE(SUM(prompt_count), 0) FROM session_registry",
                    [],
                    |r| r.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap()
    }

    #[test]
    fn ingest_from_zero_then_incremental() {
        let dir = tempfile::tempdir().unwrap();
        let hist = dir.path().join("history.jsonl");
        let l1 = history_line("hello a", 1_721_000_000_000, "/Users/x/projects/foo", "s1");
        let l2 = history_line("hello b", 1_721_000_000_100, "/Users/x/projects/foo", "s2");
        let l3 = history_line("hello a2", 1_721_000_000_200, "/Users/x/projects/foo", "s1");
        write_history(&hist, &[&l1, &l2, &l3]);

        let storage = Storage::open_memory().unwrap();
        let stats = ingest_history(&storage, &hist).unwrap();
        assert_eq!(stats.lines_read, 3);
        assert_eq!(stats.sessions_upserted, 2);
        assert_eq!(stats.parse_errors, 0);

        let s1 = registry_row(&storage, "s1").unwrap();
        assert_eq!(s1.0, "foo");
        assert_eq!(s1.4, 2); // prompt_count
        let s2 = registry_row(&storage, "s2").unwrap();
        assert_eq!(s2.4, 1);

        let l4 = history_line("more", 1_721_000_000_300, "/Users/x/projects/bar", "s3");
        let l5 = history_line("more2", 1_721_000_000_400, "/Users/x/projects/foo", "s1");
        append_history(&hist, &[&l4, &l5]);

        let stats2 = ingest_history(&storage, &hist).unwrap();
        assert_eq!(stats2.lines_read, 2);
        assert_eq!(stats2.sessions_upserted, 2); // s3 + s1

        let s1b = registry_row(&storage, "s1").unwrap();
        assert_eq!(s1b.4, 3); // no double-count of first 3 lines
        let s3 = registry_row(&storage, "s3").unwrap();
        assert_eq!(s3.4, 1);
        assert_eq!(s3.0, "bar");
    }

    #[test]
    fn truncation_resets_offset() {
        let dir = tempfile::tempdir().unwrap();
        let hist = dir.path().join("history.jsonl");
        let long = history_line(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            1_721_000_000_000,
            "/p/proj",
            "sess-long",
        );
        let long2 = history_line(
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            1_721_000_000_100,
            "/p/proj",
            "sess-long2",
        );
        write_history(&hist, &[&long, &long2]);

        let storage = Storage::open_memory().unwrap();
        let stats = ingest_history(&storage, &hist).unwrap();
        assert_eq!(stats.lines_read, 2);
        let offset_after: u64 = storage
            .get_meta(META_OFFSET)
            .unwrap()
            .unwrap()
            .parse()
            .unwrap();
        assert!(offset_after > 0);

        // Truncate / rewrite shorter than last checkpoint offset.
        let short = history_line("short", 1_721_000_000_200, "/p/proj", "sess-short");
        write_history(&hist, &[&short]);
        assert!(std::fs::metadata(&hist).unwrap().len() < offset_after);

        let stats2 = ingest_history(&storage, &hist).unwrap();
        assert_eq!(stats2.lines_read, 1);
        assert!(registry_row(&storage, "sess-short").is_some());
    }

    #[test]
    fn replaced_file_same_size_detected() {
        let dir = tempfile::tempdir().unwrap();
        let hist = dir.path().join("history.jsonl");
        // Fixed-width lines so we can rewrite with same total byte length.
        let line_a = r#"{"display":"AAAA","timestamp":1721000000000,"project":"/p/proj","sessionId":"session-aaaa"}"#;
        write_history(&hist, &[line_a]);
        let size1 = std::fs::metadata(&hist).unwrap().len();

        let storage = Storage::open_memory().unwrap();
        let stats = ingest_history(&storage, &hist).unwrap();
        assert_eq!(stats.lines_read, 1);
        assert!(registry_row(&storage, "session-aaaa").is_some());

        // Different first line / session, same total bytes.
        let line_b = r#"{"display":"BBBB","timestamp":1721000000000,"project":"/p/proj","sessionId":"session-bbbb"}"#;
        assert_eq!(line_a.len(), line_b.len());
        write_history(&hist, &[line_b]);
        let size2 = std::fs::metadata(&hist).unwrap().len();
        assert_eq!(size1, size2);

        let stats2 = ingest_history(&storage, &hist).unwrap();
        // Head-hash mismatch forces full re-read (would be lines_read==0 if only size-checked).
        assert_eq!(stats2.lines_read, 1);
        assert!(registry_row(&storage, "session-bbbb").is_some());
    }

    #[test]
    fn crash_between_upsert_and_offset_impossible() {
        // Invariant enforced by Storage::with_transaction wrapping both upserts
        // and meta checkpoint: after a successful ingest, re-running with no
        // file changes yields lines_read==0 and unchanged registry stats.
        let dir = tempfile::tempdir().unwrap();
        let hist = dir.path().join("history.jsonl");
        let l1 = history_line("one", 1_721_000_000_000, "/p/proj", "c1");
        let l2 = history_line("two", 1_721_000_000_100, "/p/proj", "c2");
        write_history(&hist, &[&l1, &l2]);

        let storage = Storage::open_memory().unwrap();
        let s1 = ingest_history(&storage, &hist).unwrap();
        assert_eq!(s1.lines_read, 2);

        let offset1 = storage.get_meta(META_OFFSET).unwrap();
        let sum1 = sum_prompt_count(&storage);

        let s2 = ingest_history(&storage, &hist).unwrap();
        assert_eq!(s2.lines_read, 0);
        assert_eq!(s2.sessions_upserted, 0);

        let offset2 = storage.get_meta(META_OFFSET).unwrap();
        let sum2 = sum_prompt_count(&storage);
        assert_eq!(offset1, offset2);
        assert_eq!(sum1, sum2);
        assert_eq!(sum1, 2);
    }

    #[test]
    fn malformed_lines_counted_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let hist = dir.path().join("history.jsonl");
        let good1 = history_line("ok1", 1_721_000_000_000, "/p/proj", "m1");
        let good2 = history_line("ok2", 1_721_000_000_100, "/p/proj", "m2");
        write_history(&hist, &[&good1, "NOT JSON {{{", &good2]);

        let storage = Storage::open_memory().unwrap();
        let stats = ingest_history(&storage, &hist).unwrap();
        assert!(stats.parse_errors >= 1);
        assert_eq!(stats.sessions_upserted, 2);
        assert!(registry_row(&storage, "m1").is_some());
        assert!(registry_row(&storage, "m2").is_some());

        let counters = storage.get_aux_counters().unwrap();
        assert!(
            counters.contains(&("history".to_string(), 1)),
            "bump once per call: {counters:?}"
        );
    }

    #[test]
    fn first_prompt_kept_earliest_last_ts_max() {
        let dir = tempfile::tempdir().unwrap();
        let hist = dir.path().join("history.jsonl");
        // Increasing timestamps in file order for same session.
        let early = history_line("EARLIEST", 1_721_000_000_000, "/p/proj", "same");
        let mid = history_line("MIDDLE", 1_721_000_000_500, "/p/proj", "same");
        let late = history_line("LATEST", 1_721_000_001_000, "/p/proj", "same");
        write_history(&hist, &[&early, &mid, &late]);

        let storage = Storage::open_memory().unwrap();
        let stats = ingest_history(&storage, &hist).unwrap();
        assert_eq!(stats.sessions_upserted, 1);

        let row = registry_row(&storage, "same").unwrap();
        assert_eq!(row.1.as_deref(), Some("EARLIEST"));
        assert_eq!(row.4, 3);

        let expected_last = chrono::DateTime::from_timestamp_millis(1_721_000_001_000)
            .unwrap()
            .to_rfc3339();
        assert_eq!(row.3, expected_last);

        let expected_first = chrono::DateTime::from_timestamp_millis(1_721_000_000_000)
            .unwrap()
            .to_rfc3339();
        assert_eq!(row.2, expected_first);
    }

    #[test]
    fn coverage_stats_via_ingest_and_chunk() {
        // Smoke: registry alone has gap; covered more fully in storage tests.
        let dir = tempfile::tempdir().unwrap();
        let hist = dir.path().join("history.jsonl");
        write_history(
            &hist,
            &[
                &history_line("a", 1, "/p/p", "cov1"),
                &history_line("b", 2, "/p/p", "cov2"),
                &history_line("c", 3, "/p/p", "cov3"),
            ],
        );
        let storage = Storage::open_memory().unwrap();
        ingest_history(&storage, &hist).unwrap();
        storage
            .insert_chunk(
                &ConversationChunk {
                    id: "ch1".into(),
                    conversation_id: "cov1".into(),
                    project_name: "p".into(),
                    timestamp: "2026-01-01T00:00:00Z".into(),
                    content: "x".into(),
                    message_count: 1,
                    summary: None,
                    author: Speaker::User,
                    seq: 0,
                    is_sidechain: false,
                },
                &[0.0; 4],
            )
            .unwrap();
        assert_eq!(storage.coverage_stats().unwrap(), (3, 1, 2));
    }
}
