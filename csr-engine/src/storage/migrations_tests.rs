use super::*;
use std::collections::BTreeSet;

const DROP_CURRENT_FTS: &str = "
        DROP TRIGGER IF EXISTS chunks_fts_ai;
        DROP TRIGGER IF EXISTS chunks_fts_ad;
        DROP TRIGGER IF EXISTS chunks_fts_au;
        DROP TABLE chunks_fts;
    ";

fn fts_schema(conn: &Connection) -> String {
    conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'chunks_fts'",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

fn matching_live_ids(conn: &Connection, query: &str) -> BTreeSet<String> {
    let mut stmt = conn
        .prepare(
            "SELECT c.id FROM chunks c
                 JOIN chunks_fts fts ON fts.rowid = c.rowid
                 WHERE chunks_fts MATCH ?1",
        )
        .unwrap();
    stmt.query_map([query], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<BTreeSet<_>>>()
        .unwrap()
}

fn chunks_fts_data_block_count(conn: &Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM chunks_fts_data", [], |r| r.get(0))
        .unwrap()
}

fn floor_value(conn: &rusqlite::Connection) -> Option<String> {
    conn.query_row(
        "SELECT value FROM meta WHERE key = 'witness_verdicts_v2_floor'",
        [],
        |r| r.get(0),
    )
    .optional()
    .unwrap()
}

fn insert_verdicts(conn: &rusqlite::Connection, n: usize) {
    for _ in 0..n {
        conn.execute(
            "INSERT INTO witness_verdicts (witness_id, verdict, observed_head_oid) \
                 VALUES (1, 'anchor_obsolete', 'deadbeef')",
            [],
        )
        .unwrap();
    }
}

#[path = "migrations_tests/mixed_tests.rs"]
mod mixed_tests;

#[path = "migrations_tests/witness_tests.rs"]
mod witness_tests;
