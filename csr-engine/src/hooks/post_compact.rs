//! PostCompact capture: Claude Code's own compaction summary, stored once.
//!
//! The hook sends `compact_summary` to the daemon socket (`kind=post_compact`),
//! which stores one `chunks` row with `source='native_compact'`. With no daemon
//! the hook inserts the row itself. Either way the row is keyed so that a
//! second fire, a retry after a timeout, or a later import of the same
//! transcript row is a no-op.
//!
//! The row is `derived`: its `chunk_provenance.author` is `derived`, which is
//! neither `user` nor `assistant`, and its `min_trust` stays at the unknown
//! floor. It is recalled as context; it is never a source a claim is checked
//! against.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::Result;
use rusqlite::{Connection, OpenFlags};

use super::HookInput;

/// `chunks.source` of a captured native compaction summary.
pub const SOURCE: &str = "native_compact";
/// `chunk_provenance.author` of a captured summary.
pub const AUTHOR: &str = "derived";
/// How much of the transcript's end is searched for the summary row.
const TRANSCRIPT_TAIL: u64 = 256 * 1024;
/// The socket frame is 256 KiB; a summary beyond this is cut, not refused.
const MAX_SUMMARY_BYTES: usize = 200 * 1024;
/// Direct-insert wait for another writer when no daemon answers.
const DIRECT_BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(250);

/// Text of a transcript row's `message.content`: a string, or the text parts
/// of a block array.
fn row_text(row: &serde_json::Value) -> String {
    let content = &row["message"]["content"];
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    content
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|block| block["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// The uuid of the last `isCompactSummary` row in the transcript's final
/// 256 KiB whose text equals `summary`.
pub fn transcript_summary_uuid(transcript: &Path, summary: &str) -> Option<String> {
    let mut file = std::fs::File::open(transcript).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(TRANSCRIPT_TAIL);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.take(TRANSCRIPT_TAIL).read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let wanted = summary.trim();
    let mut found = None;
    // A tail that starts mid-file begins inside a line: skip it.
    for line in text.lines().skip(usize::from(start > 0)) {
        if !line.contains("isCompactSummary") {
            continue;
        }
        let Ok(row) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if row["isCompactSummary"].as_bool() != Some(true) || row_text(&row).trim() != wanted {
            continue;
        }
        if let Some(uuid) = row["uuid"].as_str().filter(|u| !u.is_empty()) {
            found = Some(uuid.to_string());
        }
    }
    found
}

/// The row id: the transcript row's uuid, else `pc:` + blake3(session + summary).
pub fn chunk_key(input: &HookInput, summary: &str) -> String {
    let session = input.session_id.as_deref().unwrap_or_default();
    input
        .transcript_path
        .as_deref()
        .and_then(|path| transcript_summary_uuid(Path::new(path), summary))
        .unwrap_or_else(|| {
            let mut hasher = blake3::Hasher::new();
            hasher.update(session.as_bytes());
            hasher.update(summary.as_bytes());
            format!("pc:{}", hasher.finalize().to_hex())
        })
}

fn bounded(summary: &str) -> &str {
    if summary.len() <= MAX_SUMMARY_BYTES {
        return summary;
    }
    &summary[..summary.floor_char_boundary(MAX_SUMMARY_BYTES)]
}

/// Store the summary carried by `input`. `Ok(true)` when a row was written,
/// `Ok(false)` when there was nothing to store or the row already exists.
pub fn store(conn: &Connection, input: &HookInput) -> Result<bool> {
    let session = input.session_id.as_deref().unwrap_or_default();
    let summary = bounded(input.compact_summary.as_deref().unwrap_or_default());
    if session.is_empty() || summary.trim().is_empty() {
        return Ok(false);
    }
    let key = chunk_key(input, summary);
    let project = input
        .cwd
        .as_deref()
        .and_then(crate::search::cross_project::resolve_project_from_cwd)
        .unwrap_or_else(|| "unknown".into());
    let timestamp = chrono::Utc::now().to_rfc3339();
    let tx = conn.unchecked_transaction()?;
    // The NOT EXISTS arm catches a repeat that resolved a different key (the
    // first fire had no transcript row yet, the second did).
    let inserted = tx.execute(
        "INSERT OR IGNORE INTO chunks
             (id, conversation_id, project_name, timestamp, content, message_count,
              summary, seq, is_sidechain, source)
         SELECT ?1, ?2, ?3, ?4, ?5, 1, NULL, 0, 0, ?6
         WHERE NOT EXISTS (
             SELECT 1 FROM chunks WHERE source = ?6 AND conversation_id = ?2 AND content = ?5)",
        rusqlite::params![key, session, project, timestamp, summary, SOURCE],
    )?;
    if inserted == 0 {
        return Ok(false);
    }
    tx.execute(
        "INSERT OR IGNORE INTO chunk_provenance (chunk_id, author, source_conv_id)
         VALUES (?1, ?2, ?3)",
        rusqlite::params![key, AUTHOR, session],
    )?;
    let rowid: i64 = tx.query_row("SELECT rowid FROM chunks WHERE id = ?1", [&key], |r| {
        r.get(0)
    })?;
    match crate::storage::identifier_postings::write_postings(&tx, rowid, summary) {
        Ok(_) => {}
        Err(error) if format!("{error:#}").contains("no such table") => {}
        Err(error) => return Err(error),
    }
    tx.commit()?;
    Ok(true)
}

/// No daemon answered: insert the row directly. Opens an existing database
/// only (never creates one), waits at most [`DIRECT_BUSY_TIMEOUT`] for a
/// writer, and drops the summary on any failure.
pub fn store_direct(db_path: &Path, input: &HookInput) -> bool {
    let Ok(conn) = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return false;
    };
    if conn.busy_timeout(DIRECT_BUSY_TIMEOUT).is_err() {
        return false;
    }
    store(&conn, input).unwrap_or(false)
}

/// `csr-engine hook post-compact`. Always returns; the hook never fails Claude Code.
pub fn run_client(db_path: &Path, projects_dir: &Path) {
    let started = std::time::Instant::now();
    if !super::prompt_transport::spawn_watchdog(started) {
        return;
    }
    if std::env::var("CSR_DISABLE_RECURSIVE_HOOKS").as_deref() == Ok("1") {
        return;
    }
    let mut input = super::read_stdin_json();
    let has_work = input.session_id.as_deref().is_some_and(|s| !s.is_empty())
        && input
            .compact_summary
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty());
    if !has_work {
        return;
    }
    input.cwd = Some(
        super::resolve_hook_cwd(input.cwd.as_deref())
            .to_string_lossy()
            .into_owned(),
    );
    let transport = send(db_path, projects_dir, &input);
    let line = format!(
        "CSR hook post-compact: session={} prompt_id={} trigger={} total={}ms transport={}",
        input.session_id.as_deref().unwrap_or_default(),
        input.prompt_id.as_deref().unwrap_or("-"),
        input.trigger.as_deref().unwrap_or("-"),
        started.elapsed().as_millis(),
        transport,
    );
    eprintln!("{line}");
    crate::telemetry::append_timing_line(&line);
}

/// Daemon first, then a direct insert. Returns the label for the timing line.
fn send(db_path: &Path, projects_dir: &Path, input: &HookInput) -> &'static str {
    if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        let stored = runtime.block_on(super::prompt_transport::post_compact(
            db_path,
            projects_dir,
            input.clone(),
        ));
        runtime.shutdown_background();
        if stored {
            return "socket";
        }
    }
    if store_direct(db_path, input) {
        "direct"
    } else {
        "dropped"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(session: &str, summary: &str) -> HookInput {
        HookInput {
            session_id: Some(session.into()),
            compact_summary: Some(summary.into()),
            cwd: Some("/tmp/pc-project".into()),
            trigger: Some("auto".into()),
            ..Default::default()
        }
    }

    fn rows(conn: &Connection) -> Vec<(String, String, String)> {
        let mut stmt = conn
            .prepare(
                "SELECT c.id, c.source, COALESCE(p.author, '') FROM chunks c
                 LEFT JOIN chunk_provenance p ON p.chunk_id = c.id
                 WHERE c.source = 'native_compact'",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn post_compact_twice_with_the_same_payload_stores_one_row() {
        let conn = Connection::open_in_memory().unwrap();
        crate::storage::migrations::run(&conn).unwrap();
        let payload = input("sess-1", "We fixed the retry storm in the importer.");
        assert!(store(&conn, &payload).unwrap());
        assert!(!store(&conn, &payload).unwrap());
        let stored = rows(&conn);
        assert_eq!(stored.len(), 1, "{stored:?}");
        assert!(stored[0].0.starts_with("pc:"), "{stored:?}");
        assert_eq!(stored[0].2, AUTHOR);
        // Empty summary or session stores nothing.
        assert!(!store(&conn, &input("sess-1", "   ")).unwrap());
        assert!(!store(&conn, &input("", "text")).unwrap());
        assert_eq!(rows(&conn).len(), 1);
    }

    #[test]
    fn post_compact_with_a_matching_transcript_row_is_keyed_by_its_uuid_and_stored_once() {
        let dir = tempfile::tempdir().unwrap();
        let summary = "Summary: migrated the queue to postings.";
        let transcript = dir.path().join("t.jsonl");
        let mut lines = vec![
            r#"{"type":"user","uuid":"u-0","message":{"role":"user","content":"hi"}}"#.to_string(),
            serde_json::json!({"type":"user","uuid":"other-uuid","isCompactSummary":true,
                "message":{"role":"user","content":"a different summary"}})
            .to_string(),
            serde_json::json!({"type":"user","uuid":"summary-uuid-1","isCompactSummary":true,
                "message":{"role":"user","content":[{"type":"text","text":summary}]}})
            .to_string(),
        ];
        lines.push(r#"{"type":"assistant","uuid":"u-9"}"#.into());
        std::fs::write(&transcript, lines.join("\n")).unwrap();

        let conn = Connection::open_in_memory().unwrap();
        crate::storage::migrations::run(&conn).unwrap();
        let mut with_transcript = input("sess-2", summary);
        with_transcript.transcript_path = Some(transcript.to_string_lossy().into());
        assert!(store(&conn, &with_transcript).unwrap());
        assert_eq!(rows(&conn).len(), 1);
        assert_eq!(rows(&conn)[0].0, "summary-uuid-1");
        // The same fire again, and the same summary from a fire that could not
        // see the transcript: still one row.
        assert!(!store(&conn, &with_transcript).unwrap());
        assert!(!store(&conn, &input("sess-2", summary)).unwrap());
        assert_eq!(rows(&conn).len(), 1, "{:?}", rows(&conn));
        // A transcript with no matching row falls back to the hash key.
        let mut no_match = input("sess-3", "unrelated");
        no_match.transcript_path = Some(transcript.to_string_lossy().into());
        assert!(store(&conn, &no_match).unwrap());
        assert!(rows(&conn).iter().any(|r| r.0.starts_with("pc:")));
    }

    #[test]
    fn transcript_tail_is_bounded_and_skips_the_partial_first_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.jsonl");
        let summary = "late summary";
        let mut body = String::new();
        // The only matching row sits before the last 256 KiB: not found.
        body.push_str(
            &serde_json::json!({"uuid":"early","isCompactSummary":true,
                "message":{"content":summary}})
            .to_string(),
        );
        body.push('\n');
        while body.len() < (TRANSCRIPT_TAIL as usize) + 4096 {
            body.push_str("{\"type\":\"assistant\",\"uuid\":\"filler\",\"text\":\"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\"}\n");
        }
        std::fs::write(&path, &body).unwrap();
        assert_eq!(transcript_summary_uuid(&path, summary), None);
        body.push_str(
            &serde_json::json!({"uuid":"late","isCompactSummary":true,
                "message":{"content":summary}})
            .to_string(),
        );
        std::fs::write(&path, &body).unwrap();
        assert_eq!(
            transcript_summary_uuid(&path, summary).as_deref(),
            Some("late")
        );
    }

    #[test]
    fn direct_insert_never_creates_a_database() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("csr-engine.db");
        assert!(!store_direct(&db, &input("s", "text")));
        assert!(!db.exists());
        crate::storage::Storage::open(&db).unwrap();
        assert!(store_direct(&db, &input("s", "text")));
        assert!(!store_direct(&db, &input("s", "text")));
    }
}
