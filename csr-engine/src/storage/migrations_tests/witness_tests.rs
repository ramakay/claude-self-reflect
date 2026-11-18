use super::*;

#[test]
fn witness_tables_survive_reopen_with_rows_intact() {
    // CodeRabbit PR #279: `run` executes on every `Storage::open`, so a
    // current-shape table must NOT be dropped again — that wiped every
    // `backfill_wcr_witnesses` result on the next process start and
    // silently disabled the X4 `local` tier in live use.
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run(&conn).expect("first migrations::run");
    conn.execute(
        "INSERT INTO local_bindings (project, file, scope, name) VALUES ('p', 'f.ts', 'foo', 'reject')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO edge_scope_chains (project, file, src_id, dst_id, kind, chain) \
             VALUES ('p', 'f.ts', 's', 'd', 'calls', 'foo>bar')",
        [],
    )
    .unwrap();
    run(&conn).expect("second migrations::run (simulated re-open)");
    let lb: i64 = conn
        .query_row("SELECT COUNT(*) FROM local_bindings", [], |r| r.get(0))
        .unwrap();
    let esc: i64 = conn
        .query_row("SELECT COUNT(*) FROM edge_scope_chains", [], |r| r.get(0))
        .unwrap();
    assert_eq!((lb, esc), (1, 1), "witness rows must survive re-open");
}

#[test]
fn witness_ledger_migration_idempotent() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run(&conn).expect("first migrations::run");
    // Insert BEFORE the rerun: idempotency means existing ledger rows
    // survive a second migration pass (guards against the PR #279-class
    // table-wipe regression, where a rerun recreated the table).
    conn.execute(
        "INSERT INTO witness_ledger (project, file, stamp, tier, source_kind) \
             VALUES ('p', 'f.rs', 'b3:abc', 'committed', 'backfill')",
        [],
    )
    .unwrap();
    run(&conn).expect("second migrations::run (idempotent)");
    assert!(
        conn.prepare(
            "SELECT id, project, file, symbol, span_start, span_end, stamp, tier, at_oid, \
                 source_kind, source_id, created_at FROM witness_ledger LIMIT 0"
        )
        .is_ok(),
        "witness_ledger table must exist after migration"
    );
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM witness_ledger", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "ledger rows must survive a migration rerun");
}

#[test]
fn witness_ledger_tier_check_constraint_rejects_invalid_tier() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run(&conn).unwrap();
    let ok = conn.execute(
        "INSERT INTO witness_ledger (project, file, stamp, tier, source_kind) \
             VALUES ('p', 'f', 'b3:abc', 'committed', 'backfill')",
        [],
    );
    assert!(ok.is_ok(), "valid tier must be accepted: {ok:?}");
    let bad = conn.execute(
        "INSERT INTO witness_ledger (project, file, stamp, tier, source_kind) \
             VALUES ('p', 'f', 'b3:abc', 'guess', 'backfill')",
        [],
    );
    assert!(
        bad.is_err(),
        "tier CHECK constraint must reject a non-worktree/committed value"
    );
}

#[test]
fn witness_ledger_identity_index_dedupes_duplicate_symbol_row() {
    // Symbol-level rows conflict on `idx_witness_ledger_identity`, so
    // `INSERT OR IGNORE` (what `storage::witness_ledger::insert_witness`
    // issues) must dedupe them (see the append-only invariant doc above `run`).
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run(&conn).unwrap();
    let insert = "INSERT OR IGNORE INTO witness_ledger
            (project, file, symbol, span_start, span_end, stamp, tier, at_oid, source_kind, source_id)
            VALUES ('p', 'f.rs', 'foo', 1, 3, 'b3:abc', 'committed', 'deadbeef', 'backfill', 'deadbeef')";
    conn.execute(insert, []).unwrap();
    conn.execute(insert, []).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM witness_ledger", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        count, 1,
        "duplicate symbol-level witness must be ignored, not duplicated"
    );
}

#[test]
fn witness_ledger_identity_index_dedupes_null_symbol_rows() {
    // The reason `idx_witness_ledger_identity` is a COALESCE expression
    // index rather than an inline UNIQUE(...) constraint: SQLite treats
    // every NULL as distinct in a plain UNIQUE index, but COALESCE
    // normalizes the NULL key columns, so two whole-file witnesses
    // (`symbol`/`span_start`/`span_end` all NULL) with identical
    // non-NULL columns dedupe atomically at the DB level — one row, no
    // application-level guard needed.
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run(&conn).unwrap();
    let insert = "INSERT OR IGNORE INTO witness_ledger
            (project, file, stamp, tier, at_oid, source_kind, source_id)
            VALUES ('p', 'f.rs', 'b3:abc', 'committed', 'deadbeef', 'backfill', 'deadbeef')";
    conn.execute(insert, []).unwrap();
    conn.execute(insert, []).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM witness_ledger", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        count, 1,
        "identical whole-file (NULL-key) witnesses must dedupe to one row via the identity index"
    );
}

#[test]
fn witness_verdicts_migration_idempotent() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run(&conn).expect("first migrations::run");
    // Insert BEFORE the rerun — idempotency means existing verdict events
    // survive a second migration pass (same PR #279-class table-wipe
    // regression guard as `witness_ledger_migration_idempotent`).
    conn.execute(
        "INSERT INTO witness_verdicts (witness_id, verdict, observed_head_oid) \
             VALUES (1, 'anchor_obsolete', 'deadbeef')",
        [],
    )
    .unwrap();
    run(&conn).expect("second migrations::run (idempotent)");
    assert!(
        conn.prepare(
            "SELECT id, witness_id, verdict, successor_witness_id, receipt_oid, \
                 observed_head_oid, created_at FROM witness_verdicts LIMIT 0"
        )
        .is_ok(),
        "witness_verdicts table must exist after migration"
    );
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM witness_verdicts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "verdict events must survive a migration rerun");
}

#[test]
fn witness_verdicts_v2_floor_is_zero_on_a_fresh_database() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run(&conn).unwrap();
    assert_eq!(floor_value(&conn).as_deref(), Some("0"));
}

#[test]
fn witness_verdicts_v2_floor_is_set_once_to_the_max_event_id() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run(&conn).unwrap();
    // Simulate a pre-fix database: events exist, the key does not.
    insert_verdicts(&conn, 3);
    conn.execute(
        "DELETE FROM meta WHERE key = 'witness_verdicts_v2_floor'",
        [],
    )
    .unwrap();
    run(&conn).unwrap();
    assert_eq!(floor_value(&conn).as_deref(), Some("3"));

    // Later events and later opens never move the floor.
    insert_verdicts(&conn, 2);
    run(&conn).unwrap();
    assert_eq!(floor_value(&conn).as_deref(), Some("3"));
}

#[test]
fn witness_verdicts_v2_floor_step_performs_no_write_when_the_key_exists() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run(&conn).unwrap();
    insert_verdicts(&conn, 2);
    let before = conn.total_changes();
    ensure_witness_verdicts_v2_floor(&conn).unwrap();
    ensure_witness_verdicts_v2_floor(&conn).unwrap();
    assert_eq!(
        conn.total_changes(),
        before,
        "an existing floor must be read, never rewritten"
    );
    assert_eq!(floor_value(&conn).as_deref(), Some("0"));
}

#[test]
fn witness_verdicts_verdict_check_constraint_rejects_invalid_verdict() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run(&conn).unwrap();
    let ok = conn.execute(
        "INSERT INTO witness_verdicts (witness_id, verdict, observed_head_oid) \
             VALUES (1, 'superseded_by', 'deadbeef')",
        [],
    );
    assert!(ok.is_ok(), "valid verdict must be accepted: {ok:?}");
    let bad = conn.execute(
        "INSERT INTO witness_verdicts (witness_id, verdict, observed_head_oid) \
             VALUES (1, 'guess', 'deadbeef')",
        [],
    );
    assert!(
        bad.is_err(),
        "verdict CHECK constraint must reject a non-enumerated value"
    );
}

#[test]
fn witness_verdicts_has_no_unique_identity_index() {
    // Idempotency for verdict events is APP-SIDE ONLY (compare against the
    // LATEST event per witness — see `witness_verdicts::is_new_event`). A
    // UNIQUE identity index would silently swallow legitimate state
    // re-visits (B -> A -> B) via `INSERT OR IGNORE`, so the migration
    // must not create one — and must drop it from pre-release dev DBs.
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run(&conn).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' \
                 AND name='idx_witness_verdicts_identity'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0, "no UNIQUE identity index on witness_verdicts");
    // The same row twice must therefore insert twice at the raw SQL layer
    // (the app-side latest-event check is the only dedupe).
    let insert = "INSERT INTO witness_verdicts \
            (witness_id, verdict, successor_witness_id, receipt_oid, observed_head_oid) \
            VALUES (1, 'superseded_by', 2, 'cafebabe', 'deadbeef')";
    conn.execute(insert, []).unwrap();
    conn.execute(insert, []).unwrap();
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM witness_verdicts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 2, "raw inserts are never DB-deduped");
}

#[test]
fn code_nodes_conv_indexes_exist() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run(&conn).unwrap();
    let idx_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' \
                 AND name IN ('idx_code_nodes_first_conv','idx_code_nodes_last_conv')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        idx_count, 2,
        "both code_nodes conversation-attribution indexes must exist"
    );
}

#[test]
fn worktree_backfill_rewrites_stored_paths_to_canonical() {
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("main");
    // Path must contain `/.claude/worktrees/` so the migration LIKE filter matches.
    let wt = tmp.path().join(".claude").join("worktrees").join("wt");
    std::fs::create_dir_all(main.join("src")).unwrap();
    std::fs::create_dir_all(main.join(".git").join("worktrees").join("wt")).unwrap();
    std::fs::create_dir_all(wt.join("src")).unwrap();

    let main_file = main.join("src").join("a.rs");
    std::fs::write(&main_file, "fn a() {}").unwrap();
    let gitdir_line = format!("gitdir: {}/.git/worktrees/wt\n", main.display());
    std::fs::write(wt.join(".git"), gitdir_line).unwrap();

    let wt_file = wt.join("src").join("a.rs").to_string_lossy().to_string();
    // Compare against the same resolved spelling `canonical_repo_path` stores
    // (macOS /var → /private/var via canonicalize).
    let main_file_str = std::fs::canonicalize(&main_file)
        .unwrap_or(main_file.clone())
        .to_string_lossy()
        .to_string();

    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run(&conn).expect("first migrations::run");

    conn.execute(
        "INSERT INTO code_evolution (id, session_id, project_name, file_path, language, tool_name) \
             VALUES ('evo_1', 'sess', 'proj', ?1, 'rust', 'Write')",
        rusqlite::params![wt_file],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO code_nodes (id, repo, project, file, kind, name) \
             VALUES ('node_1', 'repo', 'proj', ?1, 'function', 'foo')",
        rusqlite::params![wt_file],
    )
    .unwrap();

    backfill_worktree_paths(&conn).unwrap();

    let evo_path: String = conn
        .query_row(
            "SELECT file_path FROM code_evolution WHERE id = 'evo_1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let node_path: String = conn
        .query_row("SELECT file FROM code_nodes WHERE id = 'node_1'", [], |r| {
            r.get(0)
        })
        .unwrap();

    assert_eq!(
        evo_path, main_file_str,
        "code_evolution.file_path must be rewritten to the canonical path"
    );
    assert_ne!(
        evo_path, wt_file,
        "must not remain keyed under the worktree path"
    );
    // code_nodes must be left ALONE. Rewriting `file` without recomputing
    // the path-derived `id` would desynchronize a row from its own identity
    // and hand it to `retire_missing_nodes` as a deletion target on the next
    // extraction, destroying its attribution provenance. Asserting the
    // non-rewrite is the safety property, not a relaxation of the old one.
    assert_eq!(
        node_path, wt_file,
        "code_nodes.file must NOT be rewritten — id is derived from file, \
             so rewriting the path alone makes the row a retirement target"
    );
    assert_ne!(
        node_path, main_file_str,
        "code_nodes must not be silently re-keyed to the canonical path"
    );
}

#[test]
fn worktree_backfill_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("main2");
    // Path must contain `/.claude/worktrees/` so the migration LIKE filter matches.
    let wt = tmp.path().join(".claude").join("worktrees").join("wt2");
    std::fs::create_dir_all(main.join("src")).unwrap();
    std::fs::create_dir_all(main.join(".git").join("worktrees").join("wt2")).unwrap();
    std::fs::create_dir_all(wt.join("src")).unwrap();

    let main_file = main.join("src").join("b.rs");
    std::fs::write(&main_file, "fn b() {}").unwrap();
    let gitdir_line = format!("gitdir: {}/.git/worktrees/wt2\n", main.display());
    std::fs::write(wt.join(".git"), gitdir_line).unwrap();

    let wt_file = wt.join("src").join("b.rs").to_string_lossy().to_string();

    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run(&conn).expect("first migrations::run");

    conn.execute(
        "INSERT INTO code_evolution (id, session_id, project_name, file_path, language, tool_name) \
             VALUES ('evo_2', 'sess', 'proj', ?1, 'rust', 'Write')",
        rusqlite::params![wt_file],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO code_nodes (id, repo, project, file, kind, name) \
             VALUES ('node_2', 'repo', 'proj', ?1, 'function', 'bar')",
        rusqlite::params![wt_file],
    )
    .unwrap();

    backfill_worktree_paths(&conn).unwrap();

    let count_evo_1: i64 = conn
        .query_row("SELECT COUNT(*) FROM code_evolution", [], |r| r.get(0))
        .unwrap();
    let count_nodes_1: i64 = conn
        .query_row("SELECT COUNT(*) FROM code_nodes", [], |r| r.get(0))
        .unwrap();
    let evo_path_1: String = conn
        .query_row(
            "SELECT file_path FROM code_evolution WHERE id = 'evo_2'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let node_path_1: String = conn
        .query_row("SELECT file FROM code_nodes WHERE id = 'node_2'", [], |r| {
            r.get(0)
        })
        .unwrap();

    // Second pass, direct call (bypassing the one-shot `meta` gate on purpose,
    // to prove the underlying rewrite logic itself is idempotent).
    backfill_worktree_paths(&conn).unwrap();

    let count_evo_2: i64 = conn
        .query_row("SELECT COUNT(*) FROM code_evolution", [], |r| r.get(0))
        .unwrap();
    let count_nodes_2: i64 = conn
        .query_row("SELECT COUNT(*) FROM code_nodes", [], |r| r.get(0))
        .unwrap();
    let evo_path_2: String = conn
        .query_row(
            "SELECT file_path FROM code_evolution WHERE id = 'evo_2'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let node_path_2: String = conn
        .query_row("SELECT file FROM code_nodes WHERE id = 'node_2'", [], |r| {
            r.get(0)
        })
        .unwrap();

    assert_eq!(
        count_evo_1, count_evo_2,
        "row count must be stable across repeated backfill passes"
    );
    assert_eq!(
        count_nodes_1, count_nodes_2,
        "row count must be stable across repeated backfill passes"
    );
    assert_eq!(
        evo_path_1, evo_path_2,
        "path must be stable across repeated backfill passes"
    );
    assert_eq!(
        node_path_1, node_path_2,
        "path must be stable across repeated backfill passes"
    );
}

// ------------------------------------------------------------------
// dreams_v1 'supersession' category widen (dream backfill Stage 6)
// ------------------------------------------------------------------

#[test]
fn fresh_db_allows_supersession_category_immediately() {
    let conn = Connection::open_in_memory().unwrap();
    run(&conn).expect("first migrations::run");
    conn.execute(
        "INSERT INTO dreams_v1 (dream_id, project, category, revision_hash, prose)
             VALUES ('id1', 'p', 'supersession', 'rev1', 'card text')",
        [],
    )
    .expect("a fresh DB's dreams_v1 must accept 'supersession' without a rebuild");
}

#[test]
fn provenance_column_is_added_nullable_without_losing_current_schema_rows() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE dreams_v1 (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                dream_id TEXT NOT NULL UNIQUE,
                project TEXT NOT NULL,
                category TEXT NOT NULL CHECK (category IN ('unfinished','strategy','supersession')),
                subject_key TEXT,
                revision_hash TEXT NOT NULL,
                prose TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                status TEXT NOT NULL DEFAULT 'open'
             );
             INSERT INTO dreams_v1 (dream_id, project, category, revision_hash, prose)
             VALUES ('legacy-current', 'p', 'supersession', 'rev0', 'legacy card');",
    )
    .unwrap();

    run(&conn).expect("additive provenance migration");

    let (prose, provenance): (String, Option<String>) = conn
        .query_row(
            "SELECT prose, evidence_provenance FROM dreams_v1 WHERE dream_id = 'legacy-current'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("legacy row and nullable provenance column must remain readable");
    assert_eq!(prose, "legacy card");
    assert!(provenance.is_none());
}

#[test]
fn preexisting_two_value_check_is_rebuilt_without_losing_rows() {
    // Simulate a DB created before this migration: dreams_v1 with the
    // OLD 2-value CHECK, carrying a real row a user may have already
    // read/verdicted.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE dreams_v1 (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                dream_id TEXT NOT NULL UNIQUE,
                project TEXT NOT NULL,
                category TEXT NOT NULL CHECK (category IN ('unfinished','strategy')),
                subject_key TEXT,
                revision_hash TEXT NOT NULL,
                prose TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                status TEXT NOT NULL DEFAULT 'open'
             );
             CREATE INDEX IF NOT EXISTS idx_dreams_v1_lookup
                 ON dreams_v1(project, category, revision_hash);
             INSERT INTO dreams_v1 (dream_id, project, category, subject_key, revision_hash,
                                    prose, status)
             VALUES ('old-id', 'p', 'unfinished', 'item-1', 'rev0', 'legacy card', 'open');",
    )
    .unwrap();

    run(&conn).expect("migrations::run over a pre-existing old-schema dreams_v1");

    // The legacy row survived the rebuild, untouched.
    let (project, category, subject_key, prose, status): (
        String,
        String,
        Option<String>,
        String,
        String,
    ) = conn
        .query_row(
            "SELECT project, category, subject_key, prose, status FROM dreams_v1 WHERE dream_id = 'old-id'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .expect("legacy row must survive the rebuild");
    assert_eq!(project, "p");
    assert_eq!(category, "unfinished");
    assert_eq!(subject_key.as_deref(), Some("item-1"));
    assert_eq!(prose, "legacy card");
    assert_eq!(status, "open");

    // And the new category is now accepted.
    conn.execute(
        "INSERT INTO dreams_v1 (dream_id, project, category, revision_hash, prose)
             VALUES ('new-id', 'p', 'supersession', 'rev1', 'new card')",
        [],
    )
    .expect("'supersession' must be accepted after the rebuild");

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM dreams_v1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2, "no rows lost or duplicated by the rebuild");
}

#[test]
fn two_value_check_rebuild_preserves_prerelease_provenance() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE dreams_v1 (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                dream_id TEXT NOT NULL UNIQUE,
                project TEXT NOT NULL,
                category TEXT NOT NULL CHECK (category IN ('unfinished','strategy')),
                subject_key TEXT,
                revision_hash TEXT NOT NULL,
                prose TEXT NOT NULL,
                evidence_provenance TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                status TEXT NOT NULL DEFAULT 'open'
             );
             INSERT INTO dreams_v1
                (dream_id, project, category, revision_hash, prose, evidence_provenance)
             VALUES ('old-id', 'p', 'unfinished', 'rev0', 'legacy card', '{\"receipt\":true}');",
    )
    .unwrap();

    run(&conn).expect("rebuild prerelease table");

    let provenance: String = conn
        .query_row(
            "SELECT evidence_provenance FROM dreams_v1 WHERE dream_id = 'old-id'",
            [],
            |row| row.get(0),
        )
        .expect("non-NULL prerelease provenance must survive");
    assert_eq!(provenance, r#"{"receipt":true}"#);
}

#[test]
fn work_a_scoped_build_left_dirty_is_queued_for_the_drain() {
    let conn = Connection::open_in_memory().unwrap();
    run(&conn).unwrap();
    // What the scoped build left: a dirty file with a catch-up hint, and an
    // ordinary extracted file.
    conn.execute_batch(
        "INSERT INTO code_graph_file_state (project, file, dirty) VALUES ('p', 'stranded.rs', 1);
             INSERT INTO code_graph_file_state (project, file, dirty) VALUES ('p', 'clean.rs', 0);
             INSERT INTO meta (key, value) VALUES
                 ('graph_catchup:p\u{1f}stranded.rs', '{\"names\":[]}'),
                 ('unrelated', 'kept');",
    )
    .unwrap();
    run(&conn).unwrap();

    let pending = crate::storage::codegraph::pending_resolve_files(&conn).unwrap();
    assert_eq!(pending, vec![("p".into(), "stranded.rs".into(), 1)]);
    let hints: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM meta WHERE key LIKE 'graph_catchup:%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(hints, 0);
    let kept: String = conn
        .query_row("SELECT value FROM meta WHERE key = 'unrelated'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(kept, "kept");

    // Settled by the drain, it stays settled across a restart.
    assert!(crate::storage::codegraph::settle_resolved_gen(&conn, "p", "stranded.rs", 1).unwrap());
    run(&conn).unwrap();
    assert!(crate::storage::codegraph::pending_resolve_files(&conn)
        .unwrap()
        .is_empty());
}

#[test]
fn dreams_v1_rebuild_is_idempotent_on_rerun() {
    let conn = Connection::open_in_memory().unwrap();
    run(&conn).expect("first migrations::run");
    conn.execute(
        "INSERT INTO dreams_v1 (dream_id, project, category, revision_hash, prose)
             VALUES ('id1', 'p', 'supersession', 'rev1', 'card text')",
        [],
    )
    .unwrap();
    run(&conn).expect("second migrations::run must not re-rebuild an already-current table");
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM dreams_v1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "row must survive a rerun that is already a no-op");
}

#[test]
fn intent_events_migration_is_idempotent_on_the_same_database() {
    let conn = Connection::open_in_memory().unwrap();
    run(&conn).unwrap();
    run(&conn).unwrap();

    let objects: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name IN (
                    'intent_events',
                    'idx_intent_events_project_ts',
                    'idx_intent_events_project_symbol',
                    'intent_events_no_update',
                    'intent_events_no_delete'
                )",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(objects, 5);
    for column in ["detector", "classifier_score", "marker"] {
        assert!(has_column(&conn, "intent_events", column).unwrap());
    }
    let sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='intent_events'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let compact: String = sql.chars().filter(|ch| !ch.is_whitespace()).collect();
    assert!(compact.contains("UNIQUE(session_id,transcript_path,turn,kind,byte_start)"));
}

#[test]
fn intent_events_identity_rebuild_preserves_v1_rows() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE intent_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL, project TEXT NOT NULL, turn INTEGER NOT NULL,
                kind TEXT NOT NULL, quote TEXT NOT NULL, transcript_path TEXT NOT NULL,
                byte_start INTEGER NOT NULL, byte_end INTEGER NOT NULL,
                prior_claim TEXT NOT NULL DEFAULT '', symbol TEXT, file TEXT,
                classifier_hash TEXT NOT NULL, ts TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(session_id, turn, kind, byte_start));
             INSERT INTO intent_events
                (session_id, project, turn, kind, quote, transcript_path,
                 byte_start, byte_end, classifier_hash, ts)
             VALUES ('s', 'p', 3, 'correction', 'No.', '/tmp/child.jsonl',
                     10, 13, 'old', '2026-09-01T00:00:00Z');",
    )
    .unwrap();

    run(&conn).unwrap();
    run(&conn).unwrap();

    let row: (String, String, Option<String>) = conn
        .query_row(
            "SELECT quote, transcript_path, detector FROM intent_events WHERE session_id='s'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(row, ("No.".into(), "/tmp/child.jsonl".into(), None));
}

#[test]
fn strip_memory_tag_from_derived_reflections_repairs_existing_rows() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE reflections (
                id TEXT PRIMARY KEY,
                content TEXT NOT NULL,
                tags TEXT DEFAULT '[]',
                timestamp TEXT NOT NULL,
                created_at TEXT DEFAULT (datetime('now'))
             );",
    )
    .unwrap();
    let story_tags = serde_json::to_string(&["session_story", "project_x", "csr:memory"]).unwrap();
    conn.execute(
        "INSERT INTO reflections (id, content, tags, timestamp)
             VALUES ('story_1', 'a story', ?1, '2026-01-01T00:00:00Z')",
        [&story_tags],
    )
    .unwrap();
    let memory_tags = serde_json::to_string(&["anukriti", "csr:memory"]).unwrap();
    conn.execute(
        "INSERT INTO reflections (id, content, tags, timestamp)
             VALUES ('mem_1', 'a memory', ?1, '2026-01-01T00:00:00Z')",
        [&memory_tags],
    )
    .unwrap();

    migrate_strip_memory_tag_from_derived_reflections(&conn).unwrap();

    let story_after: String = conn
        .query_row("SELECT tags FROM reflections WHERE id='story_1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    let tags: Vec<String> = serde_json::from_str(&story_after).unwrap();
    assert!(
        !tags.iter().any(|t| t == "csr:memory"),
        "derived row must lose the explicit-memory stamp: {tags:?}"
    );
    assert!(tags.iter().any(|t| t == "session_story"), "{tags:?}");

    let memory_after: String = conn
        .query_row("SELECT tags FROM reflections WHERE id='mem_1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    let memory_tags_after: Vec<String> = serde_json::from_str(&memory_after).unwrap();
    assert!(
        memory_tags_after.iter().any(|t| t == "csr:memory"),
        "an explicit memory (no derived tag) must keep its stamp: {memory_tags_after:?}"
    );

    // Idempotency: re-adding the tag by hand and re-running must be a
    // no-op — the meta key gates the scan to once.
    conn.execute(
        "UPDATE reflections SET tags = ?1 WHERE id = 'story_1'",
        [&story_tags],
    )
    .unwrap();
    migrate_strip_memory_tag_from_derived_reflections(&conn).unwrap();
    let story_after_second: String = conn
        .query_row("SELECT tags FROM reflections WHERE id='story_1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        story_after_second, story_tags,
        "second run is gated by the meta key and must not rescan"
    );
}

#[test]
fn witness_verdicts_receipt_committed_at_is_added_to_an_old_table_once() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    // A table as it existed before the column.
    conn.execute_batch(
        "CREATE TABLE witness_verdicts (
                id INTEGER PRIMARY KEY,
                witness_id INTEGER NOT NULL,
                verdict TEXT NOT NULL,
                successor_witness_id INTEGER,
                receipt_oid TEXT,
                observed_head_oid TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            INSERT INTO witness_verdicts (witness_id, verdict, observed_head_oid)
            VALUES (1, 'anchor_obsolete', 'head');",
    )
    .unwrap();
    assert!(!has_column(&conn, "witness_verdicts", "receipt_committed_at").unwrap());
    ensure_witness_verdicts_receipt_committed_at(&conn).unwrap();
    assert!(has_column(&conn, "witness_verdicts", "receipt_committed_at").unwrap());
    let existing: Option<String> = conn
        .query_row(
            "SELECT receipt_committed_at FROM witness_verdicts",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(existing, None, "old events stay NULL, never backfilled");
}

#[test]
fn witness_verdicts_receipt_committed_at_step_performs_no_write_when_the_column_exists() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    run(&conn).unwrap();
    assert!(has_column(&conn, "witness_verdicts", "receipt_committed_at").unwrap());
    let before = conn.total_changes();
    ensure_witness_verdicts_receipt_committed_at(&conn).unwrap();
    ensure_witness_verdicts_receipt_committed_at(&conn).unwrap();
    run(&conn).unwrap();
    assert_eq!(
        conn.total_changes(),
        before,
        "an existing column is only read"
    );
}

/// Rows the per-file queue of an earlier build left behind become snapshots
/// without content, numbered in (old seq, project, file) order: the old
/// `seq` was each file's `graph_gen`, so two files both at 1 must not
/// collide, and a second migration must not move them.
#[test]
fn a_legacy_extract_queue_becomes_snapshots_the_drain_reads_from_disk() {
    let dir = tempfile::TempDir::new().unwrap();
    let file = dir.path().canonicalize().unwrap().join("queued.rs");
    std::fs::write(
        &file,
        "fn queued_fn() { helper_fn(); }\nfn helper_fn() {}\n",
    )
    .unwrap();
    let file = file.to_string_lossy().into_owned();
    let other = dir.path().join("gone.rs").to_string_lossy().into_owned();

    let conn = Connection::open(dir.path().join("csr-engine.db")).unwrap();
    run(&conn).unwrap();
    conn.execute_batch(
        "CREATE TABLE code_graph_extract_queue (
                 project TEXT NOT NULL, file TEXT NOT NULL, read_path TEXT NOT NULL,
                 conv_id TEXT NOT NULL DEFAULT '', session_id TEXT NOT NULL DEFAULT '',
                 seq INTEGER NOT NULL, PRIMARY KEY (project, file));",
    )
    .unwrap();
    for (path, seq) in [(&file, 1), (&other, 1)] {
        conn.execute(
            "INSERT INTO code_graph_extract_queue VALUES ('proj', ?1, ?1, 'conv-old', 'sess-old', ?2)",
            rusqlite::params![path, seq],
        )
        .unwrap();
    }
    run(&conn).unwrap();
    let legacy_left: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = 'code_graph_extract_queue')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!legacy_left, "the old table is gone");
    let seqs: Vec<i64> = conn
        .prepare("SELECT seq FROM code_graph_snapshots ORDER BY seq")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(seqs, vec![1, 2]);
    run(&conn).unwrap();
    let again: i64 = conn
        .query_row("SELECT COUNT(*) FROM code_graph_snapshots", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(again, 2, "a second run changes nothing");

    // The drain reads the file from disk, skips the one that is gone, and
    // resolves the project.
    let report =
        crate::extraction::graph_drain::drain(&conn, &|f: &str| std::path::Path::new(f).exists())
            .unwrap();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let nodes: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM code_nodes WHERE file = ?1 AND kind = 'function'",
            [&file],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(nodes, 2);
    let waiting: i64 = conn
        .query_row("SELECT COUNT(*) FROM code_graph_snapshots", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(waiting, 0);
    let pending: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM code_graph_file_state WHERE graph_gen > resolved_gen",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pending, 0, "settled");
}

/// The settled table and the bump that gives every project its one pass are
/// atomic: when the bump cannot run, the table is not left behind (a reopen
/// would see it, skip the bump, and a repo-only project would never be
/// resolved). Proves the two statements commit or roll back together; it
/// does not kill a process, it makes the second statement fail.
#[test]
fn the_settled_table_is_never_left_without_its_bump() {
    let conn = Connection::open_in_memory().unwrap();
    run(&conn).unwrap();
    conn.execute_batch(
        "INSERT INTO repo_defs (project, file, name, kind) VALUES ('scan-only', 'x.rs', 'f', 'function');
             DROP TABLE code_graph_project_settled;
             DELETE FROM code_graph_project_gen;
             ALTER TABLE repo_defs RENAME TO repo_defs_away;",
    )
    .unwrap();
    let table_exists = |c: &Connection| -> bool {
        c.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = 'code_graph_project_settled')",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    // The bump reads `repo_defs`, which is gone: the step fails ...
    assert!(create_settled_table(&conn).is_err());
    // ... and leaves nothing behind.
    assert!(!table_exists(&conn), "table committed without its bump");

    // The next open can do it whole.
    conn.execute_batch("ALTER TABLE repo_defs_away RENAME TO repo_defs;")
        .unwrap();
    create_settled_table(&conn).unwrap();
    assert!(table_exists(&conn));
    let gen: i64 = conn
        .query_row(
            "SELECT gen FROM code_graph_project_gen WHERE project = 'scan-only'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(gen >= 1);
    // And a later call changes nothing.
    create_settled_table(&conn).unwrap();
    let again: i64 = conn
        .query_row(
            "SELECT gen FROM code_graph_project_gen WHERE project = 'scan-only'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(gen, again);
}
