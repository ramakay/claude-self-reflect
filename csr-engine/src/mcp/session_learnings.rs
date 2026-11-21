//! `get_session_learnings`: the reflections stored under a session's tag, or,
//! when none were stored, the session's own indexed entries, or the reason it
//! has neither.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;

use crate::storage::Storage;

/// Most entries listed from the indexed session.
const MAX_ENTRIES: usize = 40;
/// Characters of each entry shown.
const ENTRY_CHARS: usize = 160;

/// Get learnings for a specific session.
pub fn get_session_learnings(
    storage: &Arc<Storage>,
    session_id: &str,
    limit: usize,
) -> Result<String> {
    let tag = format!("session_{}", session_id);
    let results = storage.get_reflections_by_tag(&tag, limit)?;

    let learnings: Vec<(String, Vec<String>, String)> = results
        .into_iter()
        .map(|(_id, content, tags, timestamp)| (content, tags, timestamp))
        .collect();

    Ok(crate::format::format_session_learnings(
        session_id, &learnings,
    ))
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Entries of the session read from its indexed chunks, newest first:
/// `(total chunks, [(timestamp, text)])`.
fn indexed_entries(
    storage: &Storage,
    session_id: &str,
    shown: usize,
) -> Result<(usize, Vec<(String, String)>)> {
    storage.with_connection(|conn| {
        let total: i64 = conn.query_row(
            "SELECT count(*) FROM chunks WHERE conversation_id = ?1",
            [session_id],
            |row| row.get(0),
        )?;
        // What the person asked says most about a session: list those chunks
        // first, and every chunk only when the session has none of them.
        for asks_only in [true, false] {
            let sql = format!(
                "SELECT c.timestamp, c.content FROM chunks c {} WHERE c.conversation_id = ?1 \
                 ORDER BY c.timestamp DESC, c.rowid DESC LIMIT ?2",
                if asks_only {
                    "JOIN chunk_provenance p ON p.chunk_id = c.id AND p.author = 'user'"
                } else {
                    ""
                }
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map(rusqlite::params![session_id, shown as i64], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if !rows.is_empty() || !asks_only {
                return Ok((total.max(0) as usize, rows));
            }
        }
        unreachable!("the unfiltered pass always returns")
    })
}

/// The learnings of `session_id`, else its indexed entries, else why it has
/// neither. `None` when no session of that id is known anywhere, so the caller
/// can answer with an error rather than an empty list.
pub fn session_learnings_or_entries(
    storage: &Arc<Storage>,
    projects_dir: &Path,
    session_id: &str,
    limit: usize,
) -> Result<Option<String>> {
    let tag = format!("session_{session_id}");
    if !storage.get_reflections_by_tag(&tag, limit)?.is_empty() {
        return get_session_learnings(storage, session_id, limit).map(Some);
    }
    let shown = limit.clamp(1, MAX_ENTRIES);
    let (total, rows) = indexed_entries(storage, session_id, shown)?;
    if total > 0 {
        let mut out = String::from("<session_learnings>\n");
        out.push_str(&format!(
            "  <session_id>{}</session_id>\n",
            escape(session_id)
        ));
        out.push_str(&format!("  <count>{}</count>\n", rows.len()));
        out.push_str("  <source>indexed entries of the session, newest first</source>\n");
        out.push_str(&format!(
            "  <entries total=\"{total}\" shown=\"{}\">\n",
            rows.len()
        ));
        for (timestamp, content) in &rows {
            // Scrub before the cut: a credential cut in half no longer matches
            // the pattern the reply's own redaction looks for.
            let scrubbed = crate::redact::scrub_inline(content);
            let flat = scrubbed.split_whitespace().collect::<Vec<_>>().join(" ");
            out.push_str(&format!(
                "    <entry ts=\"{}\">{}</entry>\n",
                escape(crate::format::truncate_chars(timestamp, 25)),
                escape(crate::format::truncate_chars(&flat, ENTRY_CHARS)),
            ));
        }
        out.push_str("  </entries>\n</session_learnings>\n");
        return Ok(Some(out));
    }
    let reason = match super::tools::find_conversation_file(projects_dir, session_id, None) {
        super::tools::ConversationLookup::Found(..) => format!(
            "The transcript of session {session_id} is on disk but not indexed yet, so it has no entries to list. Read it with csr_transcript(session=\"{session_id}\", view=\"yourasks\")."
        ),
        _ => match crate::transcript::chunks::lookup(storage, session_id, None) {
            crate::transcript::chunks::DbLookup::Known(known) => {
                crate::transcript::chunks::known_message(&known, false)
            }
            _ => return Ok(None),
        },
    };
    Ok(Some(format!(
        "<session_learnings>\n  <session_id>{}</session_id>\n  <count>0</count>\n  <reason>{}</reason>\n</session_learnings>\n",
        escape(session_id),
        escape(&reason)
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn storage_with_chunks(dir: &Path, conversation: &str, texts: &[&str]) -> Arc<Storage> {
        use crate::transcript::chunks::test_support::{add_chunk, temp_storage};
        let storage = temp_storage(dir);
        for (index, text) in texts.iter().enumerate() {
            add_chunk(
                &storage,
                &format!("{conversation}-chunk-{index}"),
                conversation,
                "proj",
                &format!("2026-11-0{}T10:00:00Z", index + 1),
                index as i64,
                "user",
                1,
                text,
            );
        }
        Arc::new(storage)
    }

    #[test]
    fn a_session_with_indexed_entries_lists_them_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let storage = storage_with_chunks(dir.path(), "sess-live-1", &["first ask", "second ask"]);
        let out = session_learnings_or_entries(&storage, dir.path(), "sess-live-1", 10)
            .unwrap()
            .expect("known session");
        assert!(out.contains("<entries total=\"2\" shown=\"2\">"), "{out}");
        assert!(
            out.find("second ask").unwrap() < out.find("first ask").unwrap(),
            "{out}"
        );
        assert!(!out.contains("No learnings stored"), "{out}");
    }

    #[test]
    fn a_credential_inside_an_entry_is_scrubbed_before_the_cut() {
        let dir = tempfile::tempdir().unwrap();
        let ask = format!(
            "{} deploy with API_TOKEN=zqvsecretvalue99 exported in the shell",
            "context ".repeat(10)
        );
        let storage = storage_with_chunks(dir.path(), "sess-live-2", &[ask.as_str()]);
        let out = session_learnings_or_entries(&storage, dir.path(), "sess-live-2", 10)
            .unwrap()
            .expect("known session");
        assert!(out.contains("deploy with API_TOKEN="), "{out}");
        assert!(!out.contains("zqvsecr"), "{out}");
    }

    #[test]
    fn an_unknown_session_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let storage = storage_with_chunks(dir.path(), "sess-live-1", &["x"]);
        let out = session_learnings_or_entries(
            &storage,
            dir.path(),
            "00000000-0000-4000-8000-000000000000",
            10,
        )
        .unwrap();
        assert!(out.is_none());
    }
}
