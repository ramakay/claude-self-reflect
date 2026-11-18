use super::*;
use crate::storage::migrations;

fn mem() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    migrations::run(&conn).unwrap();
    conn
}

/// The `DISTINCT ... LIKE` form `list_project_names` used for every prefix,
/// the oracle for its index-seek form on an empty prefix.
fn project_names_oracle(conn: &Connection, limit: i64) -> Vec<String> {
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT project_name FROM chunks
                 WHERE project_name LIKE '%' ESCAPE '\\'
                 ORDER BY project_name
                 LIMIT ?1",
        )
        .unwrap();
    stmt.query_map([limit], |row| row.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap()
}

fn test_chunk(id: &str, content: &str) -> ConversationChunk {
    ConversationChunk {
        id: id.into(),
        conversation_id: "conv-fts".into(),
        project_name: "project-fts".into(),
        timestamp: "2026-08-12T00:00:00Z".into(),
        content: content.into(),
        message_count: 1,
        summary: None,
        author: Speaker::ToolResult,
        seq: 0,
        is_sidechain: false,
    }
}

/// The original grouped join, with one deterministic path per conversation,
/// is the oracle for the streaming-aggregate form.
const UNENRICHED_ORACLE: &str = "SELECT c.conversation_id, MIN(i.file_path)
         FROM chunks c
         JOIN import_state i ON i.conversation_id = c.conversation_id
         LEFT JOIN enrichment_state e
             ON e.conversation_id = c.conversation_id AND e.enrichment_type = ?1
         WHERE e.status IS NULL OR e.status IN ('failed', 'pending')
         GROUP BY c.conversation_id
         ORDER BY MAX(c.timestamp) DESC
         LIMIT ?2";

/// The grouped scan over every chunk of the project that picked the newest
/// sessions before `newest_conversations`; the oracle for it.
fn recent_sessions_oracle(
    conn: &Connection,
    limit: usize,
    project: Option<&str>,
    exclude_headless: bool,
) -> Vec<(String, String, i64, i64, Option<String>)> {
    let headless = if exclude_headless {
        "AND c.conversation_id NOT IN (SELECT conversation_id FROM conversation_entrypoint \
             WHERE entrypoint LIKE 'sdk-%')"
    } else {
        ""
    };
    let scope = if project.is_some() {
        "c.project_name = ?1"
    } else {
        "?1 IS NULL"
    };
    let sql = format!(
        "SELECT c.conversation_id, MAX(c.timestamp), SUM(c.message_count), COUNT(*),
                (SELECT c2.summary FROM chunks c2 WHERE c2.conversation_id = c.conversation_id
                 ORDER BY c2.rowid ASC LIMIT 1)
             FROM chunks c WHERE {scope} {headless}
             GROUP BY c.conversation_id
             ORDER BY MAX(c.timestamp) DESC, c.conversation_id LIMIT ?2"
    );
    let mut stmt = conn.prepare(&sql).unwrap();
    stmt.query_map(params![project, limit as i64], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
    })
    .unwrap()
    .map(|r| r.unwrap())
    .collect()
}

/// The single query `first_chunk_prefixes` used to run: a correlated
/// subquery per chunk. It is the oracle for the per-conversation form.
fn first_chunk_prefixes_oracle(
    conn: &Connection,
    conversations: &[String],
    prefix_chars: usize,
) -> Vec<(String, String)> {
    let sql = format!(
        "SELECT c.conversation_id, substr(c.content, 1, ?)
             FROM chunks c
             WHERE c.conversation_id IN ({})
               AND c.id = (
                   SELECT f.id FROM chunks f
                   WHERE f.conversation_id = c.conversation_id
                   ORDER BY f.seq IS NULL, f.seq, f.rowid
                   LIMIT 1
               )",
        string_placeholders(conversations.len())
    );
    let prefix_chars = prefix_chars as i64;
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&prefix_chars];
    params.extend(conversations.iter().map(|id| id as &dyn rusqlite::ToSql));
    let mut stmt = conn.prepare(&sql).unwrap();
    let rows = stmt
        .query_map(params.as_slice(), |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap();
    let mut out: Vec<(String, String)> = rows.map(|r| r.unwrap()).collect();
    out.sort();
    out
}

/// Seed two projects that share a file path from different sessions.
fn seed_cross_project_shared_file(conn: &Connection) {
    insert_code_evolution(
        conn,
        "sess_a",
        "proj_a",
        "shared.rs",
        "rust",
        "Edit",
        "[]",
        "[]",
        "[]",
        "[]",
        "[]",
        "[]",
        None,
    )
    .unwrap();
    insert_code_evolution(
        conn,
        "sess_b",
        "proj_b",
        "shared.rs",
        "rust",
        "Edit",
        "[]",
        "[]",
        "[]",
        "[]",
        "[]",
        "[]",
        None,
    )
    .unwrap();
}

fn sample_memory_row(
    file_path: &str,
    project: &str,
    slug: &str,
    origin_session_id: Option<&str>,
    last_seen_scan: i64,
) -> MemoryRegistryRow {
    MemoryRegistryRow {
        file_path: file_path.into(),
        project: project.into(),
        slug: slug.into(),
        description: Some(format!("desc-{slug}")),
        mem_type: Some("user".into()),
        origin_session_id: origin_session_id.map(str::to_string),
        modified_ts: Some("2026-08-19T00:00:00Z".into()),
        file_mtime: 1_700_000_000,
        content_hash: format!("hash-{slug}"),
        links_json: "[]".into(),
        last_seen_scan,
    }
}

// ─── get_recent_chunks ───

fn recent_chunk(
    conv_id: &str,
    project: &str,
    ts: &str,
    content: &str,
    is_sidechain: bool,
) -> ConversationChunk {
    ConversationChunk {
        id: format!("{conv_id}-c0"),
        conversation_id: conv_id.into(),
        project_name: project.into(),
        timestamp: ts.into(),
        content: content.into(),
        message_count: 1,
        summary: None,
        author: Speaker::ToolResult,
        seq: 0,
        is_sidechain,
    }
}

fn insert_recent(
    conn: &Connection,
    conv_id: &str,
    project: &str,
    ts: &str,
    content: &str,
    is_sidechain: bool,
    source: &str,
) {
    let chunk = recent_chunk(conv_id, project, ts, content, is_sidechain);
    insert_chunk_with_source(conn, &chunk, &[0.1, 0.2], source).unwrap();
}

fn insert_registered_session(
    conn: &Connection,
    session_id: &str,
    project: &str,
    first_ts: &str,
    last_ts: &str,
) {
    upsert_session_registry_batch(
        conn,
        &[SessionRegistryRow {
            session_id: session_id.to_string(),
            project: project.to_string(),
            first_prompt: Some("real prompt".to_string()),
            first_ts: first_ts.to_string(),
            last_ts: last_ts.to_string(),
            prompt_count_delta: 1,
        }],
    )
    .unwrap();
}

#[path = "queries_tests/mixed_tests.rs"]
mod mixed_tests;

#[path = "queries_tests/recent_tests.rs"]
mod recent_tests;
