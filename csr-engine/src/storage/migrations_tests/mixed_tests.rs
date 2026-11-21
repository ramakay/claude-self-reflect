use super::*;

#[test]
fn vacuum_space_check_is_sized_off_live_pages_not_file_length() {
    // Break caught: requiring 2x the file length defers the reclaim forever on
    // the databases that need it most. Numbers are the measured state of the
    // reference corpus at the moment VACUUM is reached: 11,665,166,336 bytes
    // of file, of which 2,658,352 of 2,847,941 pages are already free, and a
    // finished VACUUM produced 747,130,880 bytes.
    let required = vacuum_free_space_required_from(4096, 2_847_941, 2_658_352);
    assert!(
        required >= 747_130_880,
        "must cover the compacted result, asked {required}"
    );
    assert!(
        required < 1_500_000_000,
        "must not ask for the whole pre-VACUUM file, asked {required}"
    );

    // A database with nothing to reclaim still gets a real check.
    let dense = vacuum_free_space_required_from(4096, 2_847_941, 0);
    assert!(dense > 11_000_000_000);

    // And an empty one never asks for zero.
    assert_eq!(
        vacuum_free_space_required_from(4096, 0, 0),
        64 * 1024 * 1024
    );
}

#[test]
fn fresh_databases_use_external_content_fts_with_all_sync_triggers() {
    // Break caught: an internal-content FTS table creates chunks_fts_content
    // and stores a second full copy of every chunk body.
    let conn = Connection::open_in_memory().unwrap();
    run(&conn).unwrap();
    let sql = fts_schema(&conn).to_ascii_lowercase();
    assert!(sql.contains("content='chunks'"), "schema was: {sql}");
    assert!(sql.contains("content_rowid='rowid'"), "schema was: {sql}");
    let content_shadow: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'chunks_fts_content'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(content_shadow, 0);
    let trigger_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'trigger'
                   AND name IN ('chunks_fts_ai', 'chunks_fts_ad', 'chunks_fts_au')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(trigger_count, 3);
}

#[test]
fn timestamp_only_update_does_not_touch_chunks_fts() {
    // Break caught: an `AFTER UPDATE ON chunks` (any column) trigger
    // fires the delete+reinsert pair into chunks_fts on every UPDATE,
    // including ones that never touch `content` — e.g. the WP-A
    // timestamp-restamp backfill. Scoping the trigger to
    // `UPDATE OF content` must leave chunks_fts_data's block count
    // unchanged across a timestamp-only UPDATE.
    let conn = Connection::open_in_memory().unwrap();
    run(&conn).unwrap();
    conn.execute_batch(
        "INSERT INTO chunks
                 (id, conversation_id, project_name, timestamp, content, message_count)
             VALUES
                 ('c1', 'conv-a', 'p', '2026-08-12T00:00:00Z', 'alpha beta gamma delta', 1);",
    )
    .unwrap();
    let before = chunks_fts_data_block_count(&conn);

    conn.execute(
        "UPDATE chunks SET timestamp = ?1 WHERE id = 'c1'",
        ["2026-09-01T00:00:00Z"],
    )
    .unwrap();

    let after = chunks_fts_data_block_count(&conn);
    assert_eq!(
        before, after,
        "a timestamp-only UPDATE must not touch chunks_fts_data"
    );
    // Content still searchable and matches exactly once — the row
    // itself was never deleted, just left alone.
    assert_eq!(matching_live_ids(&conn, "alpha").len(), 1);

    // A content UPDATE, by contrast, SHOULD still resync the index.
    conn.execute(
        "UPDATE chunks SET content = 'epsilon zeta' WHERE id = 'c1'",
        [],
    )
    .unwrap();
    assert!(matching_live_ids(&conn, "alpha").is_empty());
    assert_eq!(matching_live_ids(&conn, "epsilon").len(), 1);
}

#[test]
fn legacy_unscoped_au_trigger_is_upgraded_to_content_scoped() {
    // Simulates a database migrated before this fix: chunks_fts_au fires
    // on ANY update. A later `run()` (which always calls
    // migrate_chunks_fts_au_content_scoped, unlike migrate_chunks_fts
    // itself, which short-circuits once the schema+trigger COUNT already
    // match) must recreate it scoped to `UPDATE OF content` without
    // losing FTS sync.
    let conn = Connection::open_in_memory().unwrap();
    run(&conn).unwrap();
    conn.execute_batch(
        "DROP TRIGGER chunks_fts_au;
             CREATE TRIGGER chunks_fts_au AFTER UPDATE ON chunks BEGIN
                 INSERT INTO chunks_fts(chunks_fts, rowid, content)
                 VALUES ('delete', old.rowid, old.content);
                 INSERT INTO chunks_fts(rowid, content) VALUES (new.rowid, new.content);
             END;",
    )
    .unwrap();
    let legacy_sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = 'chunks_fts_au'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!is_content_scoped_au_trigger(&legacy_sql));

    run(&conn).unwrap();

    let upgraded_sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = 'chunks_fts_au'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        is_content_scoped_au_trigger(&upgraded_sql),
        "sql was: {upgraded_sql}"
    );

    // Still functionally correct after the upgrade: content updates stay synced.
    conn.execute_batch(
        "INSERT INTO chunks
                 (id, conversation_id, project_name, timestamp, content, message_count)
             VALUES
                 ('c1', 'conv-a', 'p', '2026-08-12T00:00:00Z', 'alpha beta', 1);",
    )
    .unwrap();
    conn.execute(
        "UPDATE chunks SET content = 'gamma delta' WHERE id = 'c1'",
        [],
    )
    .unwrap();
    assert!(matching_live_ids(&conn, "alpha").is_empty());
    assert_eq!(matching_live_ids(&conn, "gamma").len(), 1);
}

#[test]
fn legacy_fts_upgrade_purges_orphans_and_preserves_live_result_sets() {
    // Break caught: merely changing future writes leaves historical orphan
    // documents in the FTS corpus. Rebuild must derive the new index solely
    // from live chunks, without changing which live ids match.
    let conn = Connection::open_in_memory().unwrap();
    run(&conn).unwrap();
    conn.execute_batch(DROP_CURRENT_FTS).unwrap();
    conn.execute_batch(
        "CREATE VIRTUAL TABLE chunks_fts
                 USING fts5(content, tokenize='porter unicode61');
             INSERT INTO chunks
                 (rowid, id, conversation_id, project_name, timestamp, content, message_count)
             VALUES
                 (10, 'live-a', 'conv-a', 'p', '2026-08-12T00:00:00Z', 'alpha beta', 1),
                 (20, 'live-b', 'conv-b', 'p', '2026-08-12T00:00:00Z', 'beta gamma', 1);
             INSERT INTO chunks_fts(rowid, content) VALUES
                 (10, 'alpha beta'),
                 (20, 'beta gamma'),
                 (99, 'alpha orphanonly'),
                 (100, 'beta stale duplicate');",
    )
    .unwrap();
    let queries = ["alpha", "beta", "gamma", "orphanonly"];
    let before: Vec<_> = queries
        .iter()
        .map(|query| matching_live_ids(&conn, query))
        .collect();
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM chunks_fts_docsize", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        4
    );

    run(&conn).unwrap();

    let after: Vec<_> = queries
        .iter()
        .map(|query| matching_live_ids(&conn, query))
        .collect();
    assert_eq!(
        after, before,
        "migration must preserve every live result set"
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM chunks_fts_docsize", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2,
        "rebuild must index exactly the two live chunks"
    );
    assert!(fts_schema(&conn)
        .to_ascii_lowercase()
        .contains("content='chunks'"));
    assert_eq!(
        fts_migration_state(&conn).unwrap().as_deref(),
        Some(CHUNKS_FTS_COMPLETE),
        "startup must not serve before post-VACUUM FTS verification completes"
    );
}

#[test]
fn failed_legacy_fts_upgrade_rolls_back_to_the_searchable_old_index() {
    // Break caught: a non-transactional DROP/CREATE migration can strand a
    // killed or failed upgrade with neither a usable old nor new index.
    let conn = Connection::open_in_memory().unwrap();
    run(&conn).unwrap();
    conn.execute_batch(DROP_CURRENT_FTS).unwrap();
    conn.execute_batch(
        "ALTER TABLE chunks RENAME COLUMN content TO body;
             CREATE VIRTUAL TABLE chunks_fts
                 USING fts5(content, tokenize='porter unicode61');
             INSERT INTO chunks
                 (rowid, id, conversation_id, project_name, timestamp, body, message_count)
             VALUES (10, 'survivor', 'conv', 'p', '2026-08-12T00:00:00Z', 'stillsearchable', 1);
             INSERT INTO chunks_fts(rowid, content) VALUES (10, 'stillsearchable');",
    )
    .unwrap();

    run(&conn).expect_err("missing external content column must fail after the transactional DROP");

    let schema = fts_schema(&conn).to_ascii_lowercase();
    assert!(
        !schema.contains("content='chunks'"),
        "legacy schema must roll back: {schema}"
    );
    assert_eq!(
        matching_live_ids(&conn, "stillsearchable"),
        BTreeSet::from(["survivor".to_string()])
    );
}

#[test]
fn interrupted_post_vacuum_rebuild_is_repaired_before_reopen_returns() {
    // Break caught: after VACUUM, an implicit chunks.rowid may have moved.
    // A durable pending-rebuild marker must force reconstruction before a
    // restarted process can use the index.
    let conn = Connection::open_in_memory().unwrap();
    run(&conn).unwrap();
    conn.execute(
        "INSERT INTO chunks
                 (id, conversation_id, project_name, timestamp, content, message_count)
             VALUES ('restart', 'conv', 'p', '2026-08-12T00:00:00Z', 'restarttoken', 1)",
        [],
    )
    .unwrap();
    let rowid: i64 = conn
        .query_row("SELECT rowid FROM chunks WHERE id = 'restart'", [], |r| {
            r.get(0)
        })
        .unwrap();
    conn.execute(
        "INSERT INTO chunks_fts(chunks_fts, rowid, content)
             VALUES ('delete', ?1, 'restarttoken')",
        [rowid],
    )
    .unwrap();
    assert!(matching_live_ids(&conn, "restarttoken").is_empty());
    set_fts_migration_state(&conn, CHUNKS_FTS_PENDING_REBUILD).unwrap();

    run(&conn).expect("reopen must finish the interrupted rebuild");

    assert_eq!(
        matching_live_ids(&conn, "restarttoken"),
        BTreeSet::from(["restart".to_string()])
    );
    assert_eq!(
        fts_migration_state(&conn).unwrap().as_deref(),
        Some(CHUNKS_FTS_COMPLETE)
    );
}

// ---- narrative_usage.ref_id (codex X5 finding 12) --------------------

#[test]
fn ref_id_migration_creates_both_the_column_and_its_index() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run(&conn).expect("first migrations::run");
    assert!(has_column(&conn, "narrative_usage", "ref_id").unwrap());
    assert!(has_index(&conn, "narrative_usage", "idx_narrative_usage_ref").unwrap());
    run(&conn).expect("second migrations::run (idempotent)");
    assert!(has_column(&conn, "narrative_usage", "ref_id").unwrap());
    assert!(has_index(&conn, "narrative_usage", "idx_narrative_usage_ref").unwrap());
}

#[test]
fn a_partial_ref_id_schema_is_detected_and_repaired_not_assumed_migrated() {
    // The exact half-applied shape the old `let _ = execute_batch(...)`
    // accepted as complete: the column landed, the index did not. The old
    // code's probe (`SELECT ref_id FROM narrative_usage LIMIT 0`) succeeds
    // here, so it skipped the block and left the index missing forever.
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run(&conn).expect("migrations::run");
    conn.execute_batch("DROP INDEX idx_narrative_usage_ref")
        .expect("drop index to simulate an interrupted migration");
    assert!(has_column(&conn, "narrative_usage", "ref_id").unwrap());
    assert!(!has_index(&conn, "narrative_usage", "idx_narrative_usage_ref").unwrap());

    migrate_narrative_usage_ref_id(&conn).expect("repair");
    assert!(
        has_index(&conn, "narrative_usage", "idx_narrative_usage_ref").unwrap(),
        "a column-without-index schema must be repaired, not treated as migrated"
    );
}

#[test]
fn a_missing_narrative_usage_table_fails_the_migration_instead_of_being_swallowed() {
    // Disk failure / dropped table stands in for any reason the ALTER
    // cannot apply. The point is that it is an Err, not a silent no-op
    // that leaves every later ref_id insert failing.
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    let error = migrate_narrative_usage_ref_id(&conn)
        .expect_err("no narrative_usage table — the migration must fail loudly");
    assert!(
        error.to_string().contains("narrative_usage"),
        "the error must name what failed: {error}"
    );
}

#[test]
fn ref_id_migration_leaves_no_open_savepoint_behind() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run(&conn).expect("migrations::run");
    assert!(
        conn.is_autocommit(),
        "a released savepoint must leave the connection in autocommit"
    );
}

// ---- Journal v4 P4b attribution + Wave 3 reservations -----------------

#[test]
fn dream_attributions_table_exists_with_every_documented_column() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run(&conn).expect("first migrations::run");
    run(&conn).expect("second migrations::run (idempotent)");
    assert!(
        conn.prepare(
            "SELECT dream_id, kind, emitted_at, bound_session_id, bound_at,
                        outcome_episode_id, outcome, receipts_json
                 FROM dream_attributions LIMIT 0"
        )
        .is_ok(),
        "dream_attributions must carry every column the design names"
    );
}

#[test]
fn an_attribution_row_with_neither_a_kind_nor_a_binding_is_rejected() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run(&conn).expect("migrations::run");
    let result = conn.execute(
        "INSERT INTO dream_attributions (dream_id) VALUES ('deadbeef')",
        [],
    );
    assert!(
        result.is_err(),
        "a row that is neither an emission (kind) nor a binding (session) claims nothing"
    );
}

#[test]
fn narrative_reservations_table_exists_and_constrains_its_states() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run(&conn).expect("first migrations::run");
    run(&conn).expect("second migrations::run (idempotent)");
    conn.execute(
        "INSERT INTO narrative_reservations (attempt_key, call_site) VALUES ('k1', 'dream_plan')",
        [],
    )
    .expect("a reservation is writable before the call");
    let bad = conn.execute(
        "UPDATE narrative_reservations SET state = 'probably_fine' WHERE attempt_key = 'k1'",
        [],
    );
    assert!(bad.is_err(), "the state vocabulary is closed");
    let dup = conn.execute(
        "INSERT INTO narrative_reservations (attempt_key, call_site) VALUES ('k1', 'dream_plan')",
        [],
    );
    assert!(dup.is_err(), "attempt_key is the idempotency key");
}

#[test]
fn saga_columns_migration_idempotent() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run(&conn).expect("first migrations::run");
    run(&conn).expect("second migrations::run (idempotent)");
    assert!(
        conn.prepare("SELECT seq, is_sidechain FROM chunks LIMIT 0")
            .is_ok(),
        "seq and is_sidechain columns must exist after migration"
    );
}

#[test]
fn artifact_receipts_survive_replacement_of_source_chunks() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    run(&conn).unwrap();
    conn.execute_batch("INSERT INTO chunks(id,conversation_id,project_name,timestamp,content,message_count) VALUES('c','s','p','now','old',1);
            INSERT INTO artifact_derivations VALUES('reflection','r',0,3,NULL,'c',0,3);
            DELETE FROM chunks WHERE id='c';").unwrap();
    let support: String = conn
        .query_row(
            "SELECT support_chunk_id FROM artifact_derivations",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        support, "c",
        "a missing parent remains countable and can lower descendants to Unknown"
    );
}

#[test]
fn artifact_floors_default_unknown_and_migrate_legacy_rows() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE reflections (id TEXT PRIMARY KEY, content TEXT NOT NULL, tags TEXT NOT NULL, timestamp TEXT NOT NULL, created_at TEXT DEFAULT (datetime('now')));
            INSERT INTO reflections(id,content,tags,timestamp) VALUES('old','unchanged','[\"source:user\"]','2026-01-01');").unwrap();
    run(&conn).unwrap();
    run(&conn).unwrap();
    for table in [
        "reflections",
        "witness_verdicts",
        "dream_threads",
        "dream_plans",
        "dreams_v1",
        "intent_events",
        "resolution_ledger",
        "resolution_proposals",
        "episode_index",
        "dream_relations",
        "derivation_ledger",
        "journal_headlines",
        "session_instrumentation",
        "ratification_scores",
    ] {
        let column: (i64, String) = conn
            .query_row(
                "SELECT \"notnull\", dflt_value FROM pragma_table_info(?1) WHERE name='min_trust'",
                [table],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(column, (1, "0".into()), "{table}");
    }
    let row: (String, i64) = conn
        .query_row(
            "SELECT content,min_trust FROM reflections WHERE id='old'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(row, ("unchanged".into(), 0));
    assert!(conn
        .prepare("SELECT event_id FROM resolution_ledger")
        .is_ok());
    assert!(conn.prepare("SELECT artifact_kind,artifact_id,artifact_start_char,artifact_end_char,support_event_id,support_chunk_id,support_start_char,support_end_char FROM artifact_derivations").is_ok());
}

#[test]
fn artifact_floor_maintenance_preserves_intent_and_confirmation_immutability() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run(&conn).unwrap();
    conn.execute_batch("INSERT INTO intent_events(session_id,project,turn,kind,quote,transcript_path,byte_start,byte_end,classifier_hash,ts) VALUES('s','p',1,'correction','no','/a',0,2,'h','now');
          INSERT INTO provenance_events VALUES('confirm','s','digest',0,'user_confirmation',4,NULL,'elicitation_digest','digest','now');").unwrap();
    conn.execute("UPDATE intent_events SET min_trust=3", [])
        .unwrap();
    assert!(conn
        .execute("UPDATE intent_events SET quote='yes'", [])
        .is_err());
    assert!(conn
        .execute("UPDATE intent_events SET classifier_score='forged'", [])
        .is_err());
    assert!(conn.execute("DELETE FROM intent_events", []).is_err());
    assert!(conn
        .execute(
            "UPDATE provenance_events SET trust_tier=5 WHERE event_id='confirm'",
            []
        )
        .is_err());
    assert!(conn
        .execute("DELETE FROM provenance_events WHERE event_id='confirm'", [])
        .is_err());
    assert!(conn
        .execute(
            "INSERT INTO artifact_derivations VALUES('reflection','x',0,2,NULL,NULL,0,2)",
            []
        )
        .is_err());
    assert!(conn
        .execute(
            "INSERT INTO artifact_derivations VALUES('reflection','x',2,1,'confirm',NULL,0,2)",
            []
        )
        .is_err());
    conn.execute(
        "INSERT INTO artifact_derivations VALUES('reflection','x',0,2,'confirm',NULL,0,2)",
        [],
    )
    .unwrap();
}

// ---- migrations on an up-to-date database write nothing ----------------
//
// Every hook process opens storage, so `run` executes several times at
// session start (session-start, session-briefing, the MCP server) and on
// every prompt. An unconditional DROP+CREATE of `intent_events_no_update`
// made each of those opens a schema write: concurrent opens raced between
// the DROP and the CREATE ("trigger intent_events_no_update already
// exists"), and any open behind a long daemon write waited out the busy
// timeout and died with "database is locked".

#[test]
fn rerunning_migrations_on_a_migrated_database_leaves_the_schema_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("csr.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    run(&conn).unwrap();
    let schema_version = |conn: &Connection| -> i64 {
        conn.query_row("PRAGMA schema_version", [], |row| row.get(0))
            .unwrap()
    };
    let before = schema_version(&conn);
    run(&conn).unwrap();
    assert_eq!(schema_version(&conn), before);
}

#[test]
fn migrations_on_a_migrated_database_do_not_wait_for_another_writer() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("csr.db");
    let writer = rusqlite::Connection::open(&path).unwrap();
    writer.execute_batch("PRAGMA journal_mode=WAL").unwrap();
    run(&writer).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();

    let opener = rusqlite::Connection::open(&path).unwrap();
    opener.busy_timeout(std::time::Duration::ZERO).unwrap();
    run(&opener).expect("an up-to-date database must migrate without the write lock");
    writer.execute_batch("COMMIT").unwrap();
}

#[test]
fn r8_add_column_rechecks_schema_and_propagates_real_errors() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE probe(id INTEGER)")
        .unwrap();
    let ddl = "ALTER TABLE probe ADD COLUMN value TEXT";
    add_column(&conn, "probe", "value", ddl).unwrap();
    // Simulate an opener that checked before the other one added it.
    add_column(&conn, "probe", "value", ddl).unwrap();
    assert!(add_column(
        &conn,
        "missing",
        "value",
        "ALTER TABLE missing ADD COLUMN value TEXT"
    )
    .is_err());
}

#[test]
fn r8_eight_concurrent_old_schema_openers_succeed_repeatedly() {
    let temp = tempfile::tempdir().unwrap();
    let old = temp.path().join("old.db");
    {
        // Every deployed database is WAL (Storage::open sets it on creation).
        let conn = Connection::open(&old).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL").unwrap();
        run(&conn).unwrap();
        conn.execute_batch(
            "ALTER TABLE enrichment_state DROP COLUMN batch_failures;
                ALTER TABLE enrichment_state DROP COLUMN told_through;
                ALTER TABLE import_state DROP COLUMN csr_tool_blocks_suppressed;
                ALTER TABLE import_state DROP COLUMN csr_hook_wrappers_scrubbed;
                ALTER TABLE import_state DROP COLUMN secrets_redacted;
                ALTER TABLE witness_verdicts DROP COLUMN receipt_committed_at;
                ALTER TABLE rerank_models DROP COLUMN curated_baseline_score;
                ALTER TABLE rerank_models DROP COLUMN curated_trained_score;
                ALTER TABLE rerank_models DROP COLUMN curated_veto_epsilon;
                ALTER TABLE rerank_models DROP COLUMN curated_case_count;
                ALTER TABLE dreams_v1 DROP COLUMN evidence_provenance;",
        )
        .unwrap();
    }
    for repeat in 0..10 {
        let path = temp.path().join(format!("copy-{repeat}.db"));
        std::fs::copy(&old, &path).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let handles = (0..8)
            .map(|_| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    crate::storage::Storage::open(&path)
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                })
            })
            .collect::<Vec<_>>();
        let outcomes = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>();
        assert!(
            outcomes.iter().all(Result::is_ok),
            "repeat {repeat}: {outcomes:?}"
        );
    }
}

#[test]
fn concurrent_upgrades_of_the_intent_trigger_all_succeed() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("csr.db");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL").unwrap();
        run(&conn).unwrap();
        // The pre-provenance shape every existing database starts from.
        conn.execute_batch(
            "DROP TRIGGER intent_events_no_update;
                 CREATE TRIGGER intent_events_no_update
                    BEFORE UPDATE ON intent_events
                    BEGIN SELECT RAISE(ABORT, 'intent_events is append-only'); END;",
        )
        .unwrap();
    }
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let conn = rusqlite::Connection::open(&path).unwrap();
                conn.busy_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                barrier.wait();
                run(&conn).map_err(|e| e.to_string())
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap().expect("concurrent migrations::run");
    }
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("INSERT INTO intent_events(session_id,project,turn,kind,quote,transcript_path,byte_start,byte_end,classifier_hash,ts) VALUES('s','p',1,'correction','no','/a',0,2,'h','now');").unwrap();
    conn.execute("UPDATE intent_events SET min_trust=3", [])
        .expect("the upgraded trigger lets the trust cache change");
    assert!(conn
        .execute("UPDATE intent_events SET quote='yes'", [])
        .is_err());
}

#[test]
fn provenance_substrate_migration_is_idempotent_and_defaults_closed() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run(&conn).expect("first migrations::run");
    run(&conn).expect("second migrations::run (idempotent)");

    conn.execute(
        "INSERT INTO chunks
                (id, conversation_id, project_name, timestamp, content, message_count)
             VALUES ('c1', 'conv', 'project', '2026-09-04T00:00:00Z', 'body', 1)",
        [],
    )
    .unwrap();
    let cached: (i64, Option<f64>) = conn
        .query_row(
            "SELECT min_trust, tool_result_share FROM chunks WHERE id='c1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(cached, (0, None));

    let objects: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name IN (
                    'provenance_events', 'chunk_spans',
                    'idx_chunks_min_trust', 'idx_provenance_events_conversation_seq',
                    'idx_chunk_spans_chunk')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(objects, 5);
}

#[test]
fn provenance_substrate_migrates_legacy_chunks_without_rewriting_them() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE chunks (
                id TEXT PRIMARY KEY,
                conversation_id TEXT NOT NULL,
                project_name TEXT NOT NULL,
                timestamp TEXT NOT NULL,
                content TEXT NOT NULL,
                message_count INTEGER NOT NULL,
                created_at TEXT DEFAULT (datetime('now'))
             );
             INSERT INTO chunks
                (id, conversation_id, project_name, timestamp, content, message_count)
             VALUES ('legacy', 'old-conv', 'old-project', '2026-01-02T03:04:05Z',
                     'legacy text', 7);",
    )
    .unwrap();

    run(&conn).expect("migrate legacy database");
    let row: (String, String, i64, Option<f64>) = conn
        .query_row(
            "SELECT conversation_id, content, min_trust, tool_result_share
                   FROM chunks WHERE id='legacy'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(row, ("old-conv".into(), "legacy text".into(), 0, None));
}

#[test]
fn provenance_substrate_rejects_invalid_coordinates_and_tiers() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run(&conn).unwrap();
    let bad_tier = conn.execute(
        "INSERT INTO provenance_events
                (event_id, conversation_id, message_key, seq, channel, trust_tier,
                 receipt_kind, observed_at)
             VALUES ('e1', 'conv', 'm1', 0, 'user_message', 99, 'jsonl', 'now')",
        [],
    );
    assert!(bad_tier.is_err());

    conn.execute(
        "INSERT INTO chunks
                (id, conversation_id, project_name, timestamp, content, message_count)
             VALUES ('c1', 'conv', 'p', 'now', 'body', 1)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO provenance_events
                (event_id, conversation_id, message_key, seq, channel, trust_tier,
                 receipt_kind, observed_at)
             VALUES ('e1', 'conv', 'm1', 0, 'user_message', 3, 'jsonl', 'now')",
        [],
    )
    .unwrap();
    let bad_span = conn.execute(
        "INSERT INTO chunk_spans
                (chunk_id, event_id, start_char, end_char, content_hash)
             VALUES ('c1', 'e1', 4, 2, 'hash')",
        [],
    );
    assert!(bad_span.is_err());
}

#[test]
fn ratification_scores_migration_idempotent() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run(&conn).expect("first migrations::run");
    run(&conn).expect("second migrations::run (idempotent)");
    assert!(
        conn.prepare(
            "SELECT conversation_id, score, acts_json, ledger_refs, extractor_version, extracted_at FROM ratification_scores LIMIT 0"
        )
        .is_ok(),
        "ratification_scores table must exist after migration"
    );
}

#[test]
fn witness_closure_columns_migration_idempotent() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run(&conn).expect("first migrations::run");
    run(&conn).expect("second migrations::run (idempotent)");
    assert!(
        conn.prepare("SELECT callee_kind, boundary, evidence FROM code_edges LIMIT 0")
            .is_ok(),
        "callee_kind, boundary, evidence columns must exist on code_edges after migration"
    );
    assert!(
        conn.prepare("SELECT src_content_hash FROM code_edges LIMIT 0")
            .is_ok(),
        "src_content_hash column must exist on code_edges after migration (Codex round 7)"
    );
    assert!(
        conn.prepare("SELECT project, file, name, kind, lang, scanned_at FROM repo_defs LIMIT 0")
            .is_ok(),
        "repo_defs table must exist after migration"
    );
}

#[test]
fn session_instrumentation_table_exists_with_expected_columns() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run(&conn).expect("first migrations::run");
    run(&conn).expect("second migrations::run (idempotent)");
    assert!(
        conn.prepare(
            "SELECT session_id, transcript_size, transcript_mtime, error_count, \
                 steer_count, turn_count, errors_json, steers_json, computed_at \
                 FROM session_instrumentation LIMIT 0"
        )
        .is_ok(),
        "session_instrumentation table must exist with the exact §3.3(a) columns"
    );
}

#[test]
fn resolution_ledger_migration_idempotent() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run(&conn).expect("first migrations::run");
    run(&conn).expect("second migrations::run (idempotent)");
    assert!(
        conn.prepare(
            "SELECT chunk_id, status, evidence, claim, source, created_at FROM resolution_ledger LIMIT 0"
        )
        .is_ok(),
        "resolution_ledger table must exist after migration"
    );
}

#[test]
fn resolution_source_guard_preserves_legacy_agent_rows_without_upgrading_them() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE resolution_ledger (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                chunk_id TEXT NOT NULL,
                status TEXT NOT NULL CHECK(status IN ('resolved','still_open','regressed')),
                evidence TEXT NOT NULL,
                claim TEXT,
                source TEXT NOT NULL DEFAULT 'agent',
                created_at TEXT DEFAULT (datetime('now'))
             );
             INSERT INTO resolution_ledger
                (chunk_id, status, evidence, claim, source)
             VALUES ('legacy', 'resolved', 'old evidence', 'old claim', 'agent');",
    )
    .unwrap();
    let legacy_id = conn.last_insert_rowid();

    run(&conn).expect("migrate legacy ledger");

    let legacy: (i64, String) = conn
        .query_row(
            "SELECT id, source FROM resolution_ledger WHERE chunk_id = 'legacy'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(legacy, (legacy_id, "agent".to_string()));
    assert!(conn
        .execute(
            "UPDATE resolution_ledger SET source = 'user_confirmed' WHERE id = ?1",
            [legacy_id],
        )
        .is_err());
    assert!(conn
        .execute(
            "INSERT INTO resolution_ledger
                    (chunk_id, status, evidence, source)
                 VALUES ('bad', 'resolved', 'bad source', 'journal_ui')",
            [],
        )
        .is_err());
}

#[test]
fn repo_root_columns_migration_idempotent() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run(&conn).expect("first migrations::run");
    run(&conn).expect("second migrations::run (idempotent)");
    assert!(
        conn.prepare("SELECT repo_root FROM code_nodes LIMIT 0")
            .is_ok(),
        "repo_root column must exist on code_nodes after migration"
    );
    assert!(
        conn.prepare("SELECT repo_root FROM code_evolution LIMIT 0")
            .is_ok(),
        "repo_root column must exist on code_evolution after migration"
    );
}

#[test]
fn code_node_attribution_migration_idempotent() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run(&conn).expect("first migrations::run");
    run(&conn).expect("second migrations::run (idempotent)");
    assert!(
        conn.prepare(
            "SELECT node_id, channel, source_id, observed_ts, evidence FROM code_node_attribution LIMIT 0"
        )
        .is_ok(),
        "code_node_attribution table must exist after migration"
    );
}

#[test]
fn code_node_attribution_channel_check_constraint_rejects_third_channel() {
    // WP2 Stage 2, receipt R2/R3/R8: only 'transcript' and 'git' are
    // legitimate provenance channels — a third value must be rejected at
    // the DB layer, not just by convention.
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run(&conn).unwrap();
    let ok = conn.execute(
        "INSERT INTO code_node_attribution (node_id, channel, source_id) VALUES ('n1', 'transcript', 's1')",
        [],
    );
    assert!(ok.is_ok(), "valid channel must be accepted: {ok:?}");
    let bad = conn.execute(
        "INSERT INTO code_node_attribution (node_id, channel, source_id) VALUES ('n1', 'guess', 's1')",
        [],
    );
    assert!(
        bad.is_err(),
        "channel CHECK constraint must reject a non-transcript/git value"
    );
}

#[test]
fn ast_status_column_migration_idempotent_and_defaults_supported() {
    // WP2 Stage 3, receipt R4: pre-existing rows (written before this
    // migration existed) must default to 'supported' — they were, by
    // construction, successfully parsed by the extractor.
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run(&conn).expect("first migrations::run");
    run(&conn).expect("second migrations::run (idempotent)");
    assert!(
        conn.prepare("SELECT ast_status FROM code_graph_file_state LIMIT 0")
            .is_ok(),
        "ast_status column must exist on code_graph_file_state after migration"
    );
    conn.execute(
        "INSERT INTO code_graph_file_state (project, file) VALUES ('p1', 'f1')",
        [],
    )
    .unwrap();
    let status: String = conn
        .query_row(
            "SELECT ast_status FROM code_graph_file_state WHERE project = 'p1' AND file = 'f1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status, "supported");
}

#[test]
fn local_bindings_migration_idempotent() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run(&conn).expect("first migrations::run");
    run(&conn).expect("second migrations::run (idempotent)");
    assert!(
        conn.prepare("SELECT project, file, scope, name FROM local_bindings LIMIT 0")
            .is_ok(),
        "local_bindings table (with scope column) must exist after migration"
    );
    // PRIMARY KEY(project, file, scope, name) makes a repeat insert a
    // no-op (INSERT OR IGNORE), not an error — the table is a plain
    // idempotent witness set, never accumulates duplicates.
    conn.execute(
        "INSERT INTO local_bindings (project, file, scope, name) VALUES ('p', 'f.ts', 'foo', 'reject')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO local_bindings (project, file, scope, name) VALUES ('p', 'f.ts', 'foo', 'reject')",
        [],
    )
    .unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM local_bindings", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn local_bindings_migration_drops_old_three_column_schema() {
    // Finding 1/2 (X4 adversarial review): a DB migrated under the OLD
    // (project, file, name) schema must not be left stuck on it —
    // `run` must drop and recreate with the new `scope` column, not
    // silently no-op via `CREATE TABLE IF NOT EXISTS`.
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE local_bindings (
                project TEXT NOT NULL,
                file    TEXT NOT NULL,
                name    TEXT NOT NULL,
                PRIMARY KEY (project, file, name)
            );",
    )
    .unwrap();
    run(&conn).expect("migrations::run over a pre-existing old-schema table");
    assert!(
        conn.prepare("SELECT project, file, scope, name FROM local_bindings LIMIT 0")
            .is_ok(),
        "old 3-column table must be replaced with the new 4-column schema"
    );
}
