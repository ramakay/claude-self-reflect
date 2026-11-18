use super::*;
use crate::storage::Storage;

fn chunk(id: &str, content: &str) -> crate::import::ConversationChunk {
    crate::import::ConversationChunk {
        id: id.into(),
        conversation_id: "conv-fts".into(),
        project_name: "project-fts".into(),
        timestamp: "2026-08-12T00:00:00Z".into(),
        content: content.into(),
        message_count: 1,
        summary: None,
        author: crate::provenance::Speaker::ToolResult,
        seq: 0,
        is_sidechain: false,
    }
}

/// Strand `stale` at a rowid no chunk owns, by the one SQLite path that skips
/// the delete trigger: `INSERT OR REPLACE` on a connection without
/// `recursive_triggers` deletes the old row without firing the delete
/// trigger and gives the new text a new
/// rowid. Returns the stranded rowid.
fn strand(conn: &Connection, id: &str, stale: &str, fresh: &str) -> i64 {
    let replace = "INSERT OR REPLACE INTO chunks
                       (id, conversation_id, project_name, timestamp, content, message_count)
                   VALUES (?1, 'conv-fts', 'project-fts', '2026-08-12T00:00:00Z', ?2, 1)";
    conn.execute(replace, params![id, stale]).unwrap();
    let rowid: i64 = conn
        .query_row("SELECT rowid FROM chunks WHERE id = ?1", [id], |r| r.get(0))
        .unwrap();
    conn.execute_batch("PRAGMA recursive_triggers=OFF").unwrap();
    conn.execute(replace, params![id, fresh]).unwrap();
    conn.execute_batch("PRAGMA recursive_triggers=ON").unwrap();
    rowid
}

fn dangling(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM chunks_fts_docsize d
         WHERE NOT EXISTS (SELECT 1 FROM chunks c WHERE c.rowid = d.id)",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

/// FTS5's own check of the index against `chunks` (rank 1 = content too).
fn content_check(conn: &Connection) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO chunks_fts(chunks_fts, rank) VALUES ('integrity-check', 1)",
        [],
    )
}

fn highlights(conn: &Connection, query: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT highlight(chunks_fts, 0, '[', ']') FROM chunks_fts WHERE chunks_fts MATCH ?1",
    )?;
    let rows = stmt.query_map([query], |r| r.get(0))?;
    rows.collect()
}

fn instances_at(conn: &Connection, rowid: i64) -> i64 {
    conn.execute_batch(
        "CREATE VIRTUAL TABLE temp.t_vocab USING fts5vocab(main, chunks_fts, instance)",
    )
    .unwrap();
    let n = conn
        .query_row(
            "SELECT COUNT(*) FROM temp.t_vocab WHERE doc = ?1",
            [rowid],
            |r| r.get(0),
        )
        .unwrap();
    conn.execute_batch("DROP TABLE temp.t_vocab").unwrap();
    n
}

fn scratch_tables(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM temp.sqlite_master WHERE name LIKE 'csr_fts_repair%'",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

/// The stranded text uses stems that are not fixed points of the stemmer
/// ("because" indexes as "becaus"), so handing the index its own terms back
/// would delete the wrong ones; the repair must find words that stem to them.
#[test]
fn a_stranded_keyword_row_is_removed_with_exactly_its_tokens() {
    let storage = Storage::open_memory().unwrap();
    storage
        .insert_chunk(&chunk("kept", "the kept conversation"), &[0.0; 384])
        .unwrap();
    storage
        .with_connection(|conn| {
            let stale = "because strandedword conversations were generalizations of databases \
                         and farewellL, said the cached memories";
            let gone = strand(conn, "gone", stale, "replacement text");
            assert_eq!(dangling(conn), 1);
            assert!(
                content_check(conn).is_err(),
                "fixture must be a real dangling row"
            );
            let error = highlights(conn, "strandedword").unwrap_err().to_string();
            assert!(error.contains("malformed"), "{error}");

            let repair = repair_dangling(conn, false)?;
            assert_eq!(
                repair,
                FtsRepair {
                    found: 1,
                    removed: 1,
                    unresolved: 0,
                    rebuilt: false
                }
            );
            assert_eq!(dangling(conn), 0);
            assert_eq!(instances_at(conn, gone), 0);
            content_check(conn).expect("index agrees with chunks after the repair");
            assert!(highlights(conn, "strandedword")?.is_empty());
            assert_eq!(highlights(conn, "replacement")?, vec!["[replacement] text"]);
            assert_eq!(highlights(conn, "conversation")?.len(), 1);
            assert_eq!(scratch_tables(conn), 0);
            Ok(())
        })
        .unwrap();
}

#[test]
fn a_store_with_nothing_dangling_is_left_untouched() {
    let storage = Storage::open_memory().unwrap();
    storage
        .insert_chunk(&chunk("kept", "kept words"), &[0.0; 384])
        .unwrap();
    storage
        .with_connection(|conn| {
            assert_eq!(repair_dangling(conn, true)?, FtsRepair::default());
            assert_eq!(scratch_tables(conn), 0);
            content_check(conn)?;
            Ok(())
        })
        .unwrap();
}

/// One bounded pass takes `cap` rowids; the rest wait for the next pass, or
/// for the rebuild a foreground caller allows.
#[test]
fn a_pass_stops_at_its_cap_and_only_a_foreground_caller_rebuilds() {
    let storage = Storage::open_memory().unwrap();
    storage
        .with_connection(|conn| {
            strand(conn, "a", "first stranded text", "a now");
            strand(conn, "b", "second stranded text", "b now");
            assert_eq!(dangling(conn), 2);

            let pass = repair_dangling_capped(conn, false, 1)?;
            assert_eq!((pass.found, pass.removed, pass.rebuilt), (1, 1, false));
            assert_eq!(dangling(conn), 1);

            strand(conn, "c", "third stranded text", "c now");
            let pass = repair_dangling_capped(conn, true, 1)?;
            assert!(pass.rebuilt, "{pass:?}");
            assert_eq!(dangling(conn), 0);
            content_check(conn)?;
            assert!(highlights(conn, "stranded")?.is_empty());
            Ok(())
        })
        .unwrap();
}

/// What build E stored (unhealthy, one dangling row, checked just now): the
/// daemon's refresh keeps counting it and never repairs it; `status --deep`,
/// the command the unhealthy reason names, removes it through its own writer
/// while status itself reads on a read-only connection.
#[test]
fn the_refresh_counts_dangling_rows_and_status_deep_removes_them() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("csr-engine.db");
    let storage = Storage::open(&db).unwrap();
    storage
        .with_connection(|conn| {
            strand(conn, "gone", "orphaned because text", "live text");
            Ok(())
        })
        .unwrap();
    storage.set_meta("integrity_ok", "0").unwrap();
    storage.set_meta("integrity_fts_dangling", "1").unwrap();
    storage
        .set_meta("integrity_checked_at", &chrono::Utc::now().to_rfc3339())
        .unwrap();
    assert!(!storage.integrity_check_cached(24, true).unwrap());
    let stale = chrono::Utc::now() - chrono::Duration::hours(48);
    storage
        .set_meta("integrity_checked_at", &stale.to_rfc3339())
        .unwrap();
    assert!(!storage.integrity_check_cached(24, true).unwrap());
    storage
        .with_connection(|conn| {
            assert_eq!(dangling(conn), 1, "the refresh must not repair");
            Ok(())
        })
        .unwrap();

    let reader = Storage::open_read_only(&db).unwrap();
    assert_eq!(reader.status_integrity(&db, true), (Some(true), Some(0)));
    assert_eq!(reader.status_integrity(&db, false), (Some(true), Some(0)));
    storage
        .with_connection(|conn| {
            assert_eq!(dangling(conn), 0);
            content_check(conn)?;
            Ok(())
        })
        .unwrap();
}

/// Between verification and the write, something else touched the rowid:
/// the original orphan went and a different text was stranded at the same
/// rowid. Its docsize no longer matches, so the old text is never deleted
/// against it and the new orphan's postings stay whole.
#[test]
fn a_rowid_whose_docsize_changed_after_verification_is_left_alone() {
    let storage = Storage::open_memory().unwrap();
    storage
        .with_connection(|conn| {
            let gone = strand(conn, "gone", "orphaned because text", "live text");
            let schema = chunks_fts_schema(conn)?.unwrap();
            let verified = prepare(conn, &schema, &[gone]);
            conn.execute_batch(SCRATCH)?;
            let verified = verified?;
            assert_eq!(verified.len(), 1);

            conn.execute(
                "INSERT INTO chunks_fts(chunks_fts, rowid, content) VALUES ('delete', ?1, ?2)",
                params![gone, verified[0].text],
            )?;
            conn.execute(
                "INSERT INTO chunks_fts(rowid, content)
                 VALUES (?1, 'another stranded text entirely here now')",
                [gone],
            )?;
            assert_eq!(dangling(conn), 1);

            assert_eq!(delete_verified(conn, &verified)?, 0);
            assert_eq!(dangling(conn), 1);
            assert_eq!(instances_at(conn, gone), 6);
            let matched: i64 = conn.query_row(
                "SELECT rowid FROM chunks_fts WHERE chunks_fts MATCH 'entirely'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(matched, gone);
            Ok(())
        })
        .unwrap();
}

/// Deletes commit in batches of at most `BATCH_ROWS`.
#[test]
fn more_rows_than_one_batch_are_all_removed() {
    let storage = Storage::open_memory().unwrap();
    storage
        .with_connection(|conn| {
            for i in 0..(BATCH_ROWS + 3) {
                strand(
                    conn,
                    &format!("c{i}"),
                    &format!("stale number{i} text"),
                    "now",
                );
            }
            assert_eq!(dangling(conn), BATCH_ROWS as i64 + 3);
            let repair = repair_dangling(conn, false)?;
            assert_eq!(repair.removed, BATCH_ROWS as u64 + 3);
            assert_eq!(dangling(conn), 0);
            content_check(conn)?;
            Ok(())
        })
        .unwrap();
}

#[test]
fn the_declared_tokenizer_is_read_from_the_schema() {
    let external = "CREATE VIRTUAL TABLE chunks_fts USING fts5(
        content,
        content='chunks',
        content_rowid='rowid',
        tokenize='porter unicode61'
    )";
    assert_eq!(tokenize_literal(external), Some("'porter unicode61'"));
    assert_eq!(
        tokenize_literal(
            "CREATE VIRTUAL TABLE t USING fts5(c, tokenize = \"unicode61 remove_diacritics 2\")"
        ),
        Some("\"unicode61 remove_diacritics 2\"")
    );
    assert_eq!(
        tokenize_literal(
            "CREATE VIRTUAL TABLE t USING fts5(c, tokenize='unicode61 tokenchars ''-''')"
        ),
        Some("'unicode61 tokenchars ''-'''")
    );
    assert_eq!(
        tokenize_literal("CREATE VIRTUAL TABLE t USING fts5(c)"),
        None
    );
}
