use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

const CHUNKS_FTS_COMPACTION_KEY: &str = "chunks_fts_external_content_v1";
const CHUNKS_FTS_PENDING_VACUUM: &str = "pending_vacuum";
const CHUNKS_FTS_PENDING_REBUILD: &str = "pending_rebuild";
const CHUNKS_FTS_COMPLETE: &str = "complete";

const CREATE_EXTERNAL_CHUNKS_FTS: &str = "
    CREATE VIRTUAL TABLE chunks_fts USING fts5(
        content,
        content='chunks',
        content_rowid='rowid',
        tokenize='porter unicode61'
    );
";

const CREATE_CHUNKS_FTS_TRIGGERS: &str = "
    CREATE TRIGGER chunks_fts_ai AFTER INSERT ON chunks BEGIN
        INSERT INTO chunks_fts(rowid, content) VALUES (new.rowid, new.content);
    END;
    CREATE TRIGGER chunks_fts_ad AFTER DELETE ON chunks BEGIN
        INSERT INTO chunks_fts(chunks_fts, rowid, content)
        VALUES ('delete', old.rowid, old.content);
    END;
    CREATE TRIGGER chunks_fts_au AFTER UPDATE OF content ON chunks BEGIN
        INSERT INTO chunks_fts(chunks_fts, rowid, content)
        VALUES ('delete', old.rowid, old.content);
        INSERT INTO chunks_fts(rowid, content) VALUES (new.rowid, new.content);
    END;
";

/// Keyword channel for reflections. `csr_reflect_on_past` fuses semantic and
/// FTS5 ranks (RRF); with only `chunks_fts` on the keyword side a stored
/// reflection could never earn a keyword rank and lost the fusion to any chunk
/// present in both lists, even at cosine 1.0 (2026-09-22 probe, three
/// `memory-archive` rows). A standard FTS5 table keyed by id, not an
/// external-content one: `INSERT OR REPLACE` on `reflections` never fires the
/// delete trigger for the row it replaces, so the triggers delete by id first.
const CREATE_REFLECTIONS_FTS: &str = "
    CREATE VIRTUAL TABLE IF NOT EXISTS reflections_fts USING fts5(
        id UNINDEXED,
        content,
        tokenize='porter unicode61'
    );
    CREATE TRIGGER IF NOT EXISTS reflections_fts_ai AFTER INSERT ON reflections BEGIN
        DELETE FROM reflections_fts WHERE id = new.id;
        INSERT INTO reflections_fts(id, content) VALUES (new.id, new.content);
    END;
    CREATE TRIGGER IF NOT EXISTS reflections_fts_ad AFTER DELETE ON reflections BEGIN
        DELETE FROM reflections_fts WHERE id = old.id;
    END;
    CREATE TRIGGER IF NOT EXISTS reflections_fts_au AFTER UPDATE OF content ON reflections BEGIN
        DELETE FROM reflections_fts WHERE id = old.id;
        INSERT INTO reflections_fts(id, content) VALUES (new.id, new.content);
    END;
";

const DROP_CHUNKS_FTS_TRIGGERS: &str = "
    DROP TRIGGER IF EXISTS chunks_fts_ai;
    DROP TRIGGER IF EXISTS chunks_fts_ad;
    DROP TRIGGER IF EXISTS chunks_fts_au;
";

fn fts_migration_state(conn: &Connection) -> Result<Option<String>> {
    conn.query_row(
        "SELECT value FROM meta WHERE key = ?1",
        [CHUNKS_FTS_COMPACTION_KEY],
        |row| row.get(0),
    )
    .optional()
    .map_err(Into::into)
}

fn set_fts_migration_state(conn: &Connection, state: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [CHUNKS_FTS_COMPACTION_KEY, state],
    )?;
    Ok(())
}

fn chunks_fts_schema(conn: &Connection) -> Result<Option<String>> {
    conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'chunks_fts'",
        [],
        |row| row.get(0),
    )
    .optional()
    .map_err(Into::into)
}

fn is_external_chunks_fts(schema: &str) -> bool {
    let compact: String = schema
        .to_ascii_lowercase()
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    compact.contains("content='chunks'") && compact.contains("content_rowid='rowid'")
}

fn chunks_fts_trigger_count(conn: &Connection) -> Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'trigger'
           AND name IN ('chunks_fts_ai', 'chunks_fts_ad', 'chunks_fts_au')",
        [],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

fn verify_chunks_fts(conn: &Connection) -> Result<()> {
    let schema = chunks_fts_schema(conn)?
        .ok_or_else(|| anyhow::anyhow!("chunks_fts migration left no FTS table"))?;
    if !is_external_chunks_fts(&schema) {
        anyhow::bail!("chunks_fts migration did not install external-content schema");
    }
    if chunks_fts_trigger_count(conn)? != 3 {
        anyhow::bail!("chunks_fts migration did not install all three synchronization triggers");
    }
    conn.execute(
        "INSERT INTO chunks_fts(chunks_fts, rank) VALUES('integrity-check', 1)",
        [],
    )?;
    Ok(())
}

/// Replace the legacy internal-content index atomically. The legacy table,
/// all orphan documents and its duplicate content store disappear together;
/// a failure before RELEASE rolls the whole schema change back.
fn migrate_chunks_fts(conn: &Connection) -> Result<()> {
    let existing_schema = chunks_fts_schema(conn)?;
    let already_external = existing_schema
        .as_deref()
        .is_some_and(is_external_chunks_fts);
    let triggers_complete = chunks_fts_trigger_count(conn)? == 3;
    if already_external && triggers_complete {
        return Ok(());
    }

    conn.execute_batch("SAVEPOINT csr_chunks_fts_external_content")?;
    let applied = (|| -> Result<()> {
        conn.execute_batch(DROP_CHUNKS_FTS_TRIGGERS)?;
        if !already_external {
            if existing_schema.is_some() {
                conn.execute_batch("DROP TABLE chunks_fts")?;
            }
            conn.execute_batch(CREATE_EXTERNAL_CHUNKS_FTS)?;
        }
        conn.execute_batch(CREATE_CHUNKS_FTS_TRIGGERS)?;
        conn.execute("INSERT INTO chunks_fts(chunks_fts) VALUES('rebuild')", [])?;
        verify_chunks_fts(conn)?;
        set_fts_migration_state(
            conn,
            if existing_schema.is_some() && !already_external {
                CHUNKS_FTS_PENDING_VACUUM
            } else {
                CHUNKS_FTS_COMPLETE
            },
        )?;
        Ok(())
    })();
    match applied {
        Ok(()) => conn.execute_batch("RELEASE csr_chunks_fts_external_content")?,
        Err(error) => {
            let _ = conn.execute_batch(
                "ROLLBACK TO csr_chunks_fts_external_content;
                 RELEASE csr_chunks_fts_external_content",
            );
            return Err(error);
        }
    }

    verify_chunks_fts(conn)
}

fn chunks_fts_au_trigger_sql(conn: &Connection) -> Result<Option<String>> {
    conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = 'chunks_fts_au'",
        [],
        |row| row.get(0),
    )
    .optional()
    .map_err(Into::into)
}

fn is_content_scoped_au_trigger(sql: &str) -> bool {
    let compact: String = sql
        .to_ascii_lowercase()
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    compact.contains("afterupdateofcontenton")
}

/// Recreate `chunks_fts_au` scoped to `UPDATE OF content` if an
/// already-migrated database still has the unscoped `AFTER UPDATE ON chunks`
/// definition. `migrate_chunks_fts` above short-circuits once the
/// external-content schema and a trigger COUNT of 3 already hold, so it never
/// revisits an existing trigger's exact definition — this is a narrower,
/// always-checked follow-up that does.
///
/// An unscoped trigger fires the delete+reinsert `chunks_fts` pair on every
/// `UPDATE` to `chunks`, including timestamp-only restamps that never touch
/// `content`. Measured on a `/tmp` clone: 280,775 timestamp-only restamp
/// UPDATEs (`restamp_chunk_timestamp_if_content_matches`) grew
/// `chunks_fts_data` from 51,745 blocks / 202.5 MB to 65,764 blocks /
/// 257.2 MB (+54.6 MB, +27%) purely from re-inserting unchanged content.
fn migrate_chunks_fts_au_content_scoped(conn: &Connection) -> Result<()> {
    let Some(sql) = chunks_fts_au_trigger_sql(conn)? else {
        // No chunks_fts_au trigger yet — migrate_chunks_fts (run earlier in
        // `run()`) is responsible for creating the correct one from scratch.
        return Ok(());
    };
    if is_content_scoped_au_trigger(&sql) {
        return Ok(());
    }
    conn.execute_batch(
        "DROP TRIGGER chunks_fts_au;
         CREATE TRIGGER chunks_fts_au AFTER UPDATE OF content ON chunks BEGIN
             INSERT INTO chunks_fts(chunks_fts, rowid, content)
             VALUES ('delete', old.rowid, old.content);
             INSERT INTO chunks_fts(rowid, content) VALUES (new.rowid, new.content);
         END;",
    )?;
    Ok(())
}

fn main_database_path(conn: &Connection) -> Result<Option<std::path::PathBuf>> {
    let path: String = conn.query_row(
        "SELECT file FROM pragma_database_list WHERE name = 'main'",
        [],
        |row| row.get(0),
    )?;
    Ok((!path.is_empty()).then(|| std::path::PathBuf::from(path)))
}

/// Free bytes VACUUM needs before it is worth starting.
///
/// VACUUM writes a compacted copy and then transfers it back, so what it needs
/// is space for the *result*, not for a second copy of the file on disk. Sizing
/// the check off the current file length would demand ~21 GB from the database
/// this migration exists to shrink — whose owner has just watched an index eat
/// their disk — and defer the reclaim forever on exactly the machines that need
/// it. The result is bounded by the pages still in use (VACUUM packs them
/// tighter, never looser), which at this point in the state machine is a small
/// fraction of the file: the legacy index's pages are already on the freelist.
///
/// The margin covers the rewritten b-tree structure and the journal.
fn vacuum_free_space_required(conn: &Connection) -> Result<u64> {
    let page_size: i64 = conn.query_row("PRAGMA page_size", [], |row| row.get(0))?;
    let page_count: i64 = conn.query_row("PRAGMA page_count", [], |row| row.get(0))?;
    let free_pages: i64 = conn.query_row("PRAGMA freelist_count", [], |row| row.get(0))?;
    Ok(vacuum_free_space_required_from(
        page_size.max(0) as u64,
        page_count.max(0) as u64,
        free_pages.max(0) as u64,
    ))
}

fn vacuum_free_space_required_from(page_size: u64, page_count: u64, free_pages: u64) -> u64 {
    let in_use_bytes = page_count.saturating_sub(free_pages) * page_size;
    // 25% headroom plus a 64 MiB floor, so tiny databases still get a sane check.
    in_use_bytes
        .saturating_mul(5)
        .saturating_div(4)
        .saturating_add(64 * 1024 * 1024)
}

/// Physically reclaim the pages released by the legacy FTS table. VACUUM may
/// renumber implicit rowids, so the durable state machine always rebuilds and
/// verifies the external index after VACUUM and before startup may serve reads.
fn finish_chunks_fts_compaction(conn: &Connection) -> Result<()> {
    let Some(mut state) = fts_migration_state(conn)? else {
        return Ok(());
    };
    if state == CHUNKS_FTS_COMPLETE || !conn.is_autocommit() {
        return Ok(());
    }

    if state == CHUNKS_FTS_PENDING_VACUUM {
        if let Some(path) = main_database_path(conn)? {
            let database_bytes = std::fs::metadata(&path)?.len();
            let available_bytes = fs2::available_space(path.parent().unwrap_or(&path))?;
            let required_bytes = vacuum_free_space_required(conn)?;
            if available_bytes < required_bytes {
                tracing::warn!(
                    database_bytes,
                    available_bytes,
                    required_bytes,
                    "deferring chunks_fts compaction: insufficient free disk space"
                );
                return Ok(());
            }
        }

        if let Err(error) = conn.execute_batch("VACUUM") {
            // The external index created in the preceding transaction remains
            // correct. Keep the pending marker and serve search; a later open
            // retries physical compaction.
            tracing::warn!(%error, "deferring chunks_fts compaction after VACUUM failure");
            return Ok(());
        }
        // VACUUM itself is atomic, but it may renumber chunks.rowid. Persist a
        // repair marker before attempting the FTS rebuild so a killed process
        // can never serve the potentially stale index on the next open.
        set_fts_migration_state(conn, CHUNKS_FTS_PENDING_REBUILD)?;
        state = CHUNKS_FTS_PENDING_REBUILD.to_string();
    }

    if state == CHUNKS_FTS_PENDING_REBUILD {
        conn.execute_batch("SAVEPOINT csr_chunks_fts_post_vacuum_rebuild")?;
        let rebuilt = (|| -> Result<()> {
            conn.execute("INSERT INTO chunks_fts(chunks_fts) VALUES('rebuild')", [])?;
            verify_chunks_fts(conn)?;
            set_fts_migration_state(conn, CHUNKS_FTS_COMPLETE)?;
            Ok(())
        })();
        match rebuilt {
            Ok(()) => conn.execute_batch("RELEASE csr_chunks_fts_post_vacuum_rebuild")?,
            Err(error) => {
                let _ = conn.execute_batch(
                    "ROLLBACK TO csr_chunks_fts_post_vacuum_rebuild;
                     RELEASE csr_chunks_fts_post_vacuum_rebuild",
                );
                return Err(error);
            }
        }
    }
    Ok(())
}

/// Set the `witness_verdicts` v2 epoch floor exactly once.
///
/// Every event already in the table when the first binary with the fixed
/// dream join opens a database was minted by the old buggy join (branch
/// flapping, project-keyed anchors, one event per observed HEAD). The table is
/// append-only, so those events stay, but the floor
/// (`COALESCE(MAX(id), 0)` at that moment) retires them from every live
/// reader — see `witness_verdicts::LIVE_EVENT_FLOOR_SQL`. A fresh database
/// gets floor 0.
///
/// CHECK-THEN-INSERT: when the key already exists this performs a single
/// read and NO write. Migrations that write on every open took the write lock
/// in read-only hook processes and surfaced 'database is locked' (the
/// a55e868 trigger-race fix); this must never regress into that shape.
fn ensure_witness_verdicts_v2_floor(conn: &Connection) -> Result<()> {
    let exists = conn
        .query_row(
            "SELECT 1 FROM meta WHERE key = ?1",
            [super::witness_verdicts::LIVE_FLOOR_META_KEY],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if exists {
        return Ok(());
    }
    // OR IGNORE: two processes racing the first open must not clobber each
    // other (or fail) — whoever lands first sets the floor.
    conn.execute(
        "INSERT OR IGNORE INTO meta(key, value)
         SELECT ?1, CAST(COALESCE(MAX(id), 0) AS TEXT) FROM witness_verdicts",
        [super::witness_verdicts::LIVE_FLOOR_META_KEY],
    )?;
    Ok(())
}

/// Add `witness_verdicts.receipt_committed_at`: the receipt commit's
/// committer time as RFC 3339 in the committer's OWN offset
/// (`2026-09-12T22:57:11-07:00`), so a delivered conclusion can say when the
/// change HAPPENED rather than when the dreamer noticed (`created_at`), dated
/// the way `git log` dates it (first 10 chars). NULL on every event written
/// before the column existed. Text order is NOT time order across offsets;
/// `julianday()` normalizes the `+-HH:MM` suffix, so compare through it.
///
/// CHECK-THEN-ALTER: when the column exists this is one `pragma_table_info`
/// read and NO write (see `ensure_witness_verdicts_v2_floor`).
fn ensure_witness_verdicts_receipt_committed_at(conn: &Connection) -> Result<()> {
    if !has_column(conn, "witness_verdicts", "receipt_committed_at")? {
        add_column(
            conn,
            "witness_verdicts",
            "receipt_committed_at",
            "ALTER TABLE witness_verdicts ADD COLUMN receipt_committed_at TEXT;",
        )?;
    }
    Ok(())
}

/// Run all database migrations.
/// S6 episode tier: additive tables, never touching `chunks`. `episodes_fts`
/// is external-content on `episodes` with the `chunks_fts` tokenizer.
fn migrate_episodes(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS episodes (
            id TEXT NOT NULL UNIQUE,
            conversation_id TEXT NOT NULL,
            project_name TEXT NOT NULL,
            ordinal INTEGER NOT NULL,
            first_uuid TEXT NOT NULL,
            last_uuid TEXT NOT NULL,
            timestamp TEXT NOT NULL,
            text TEXT NOT NULL,
            token_est INTEGER NOT NULL,
            echo_class INTEGER NOT NULL DEFAULT 0,
            source TEXT NOT NULL DEFAULT 'conversation'
        );
        CREATE INDEX IF NOT EXISTS idx_episodes_conversation ON episodes(conversation_id, ordinal);
        CREATE INDEX IF NOT EXISTS idx_episodes_timestamp ON episodes(timestamp);
        CREATE VIRTUAL TABLE IF NOT EXISTS episodes_fts USING fts5(
            text,
            content='episodes',
            content_rowid='rowid',
            tokenize='porter unicode61'
        );
        CREATE TRIGGER IF NOT EXISTS episodes_fts_ai AFTER INSERT ON episodes BEGIN
            INSERT INTO episodes_fts(rowid, text) VALUES (new.rowid, new.text);
        END;
        CREATE TRIGGER IF NOT EXISTS episodes_fts_ad AFTER DELETE ON episodes BEGIN
            INSERT INTO episodes_fts(episodes_fts, rowid, text)
            VALUES ('delete', old.rowid, old.text);
        END;
        CREATE TABLE IF NOT EXISTS session_cards (
            conversation_id TEXT PRIMARY KEY,
            project_name TEXT NOT NULL,
            title TEXT NOT NULL DEFAULT '',
            identifiers TEXT NOT NULL DEFAULT '[]',
            decisions TEXT NOT NULL DEFAULT '[]',
            task_state TEXT NOT NULL DEFAULT '',
            receipts TEXT NOT NULL DEFAULT '[]',
            first_ts TEXT NOT NULL DEFAULT '',
            last_ts TEXT NOT NULL DEFAULT '',
            episode_count INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS episode_watermarks (
            rel_path TEXT PRIMARY KEY,
            conversation_id TEXT NOT NULL,
            source_bytes INTEGER NOT NULL,
            tail_hash TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            resume_offset INTEGER NOT NULL DEFAULT 0,
            resume_ordinal INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS episode_compact_records (
            uuid TEXT PRIMARY KEY,
            conversation_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            timestamp TEXT NOT NULL DEFAULT ''
        );
        ",
    )?;
    Ok(())
}

pub fn run(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS chunks (
            id TEXT PRIMARY KEY,
            conversation_id TEXT NOT NULL,
            project_name TEXT NOT NULL,
            timestamp TEXT NOT NULL,
            content TEXT NOT NULL,
            message_count INTEGER NOT NULL,
            created_at TEXT DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_chunks_project ON chunks(project_name);
        CREATE INDEX IF NOT EXISTS idx_chunks_timestamp ON chunks(timestamp);
        CREATE INDEX IF NOT EXISTS idx_chunks_conversation ON chunks(conversation_id);

        CREATE TABLE IF NOT EXISTS chunk_embeddings (
            chunk_id TEXT PRIMARY KEY REFERENCES chunks(id),
            embedding BLOB NOT NULL
        );

        CREATE TABLE IF NOT EXISTS reflections (
            id TEXT PRIMARY KEY,
            content TEXT NOT NULL,
            tags TEXT DEFAULT '[]',
            timestamp TEXT NOT NULL,
            created_at TEXT DEFAULT (datetime('now'))
        );

        -- Prompt-render lookups (hooks::prompt_submit): a project's newest
        -- chunk, and a reflection-tag match that never reads row bodies.
        -- Measured on a 697k-chunk / 11.9k-reflection copy at background
        -- priority: 6.4 s and 36 MB, 0.1 s and 1.9 MB.
        CREATE INDEX IF NOT EXISTS idx_chunks_project_timestamp ON chunks(project_name, timestamp);
        CREATE INDEX IF NOT EXISTS idx_reflections_tags_timestamp ON reflections(tags, timestamp);

        CREATE TABLE IF NOT EXISTS reflection_embeddings (
            reflection_id TEXT PRIMARY KEY REFERENCES reflections(id),
            embedding BLOB NOT NULL
        );

        CREATE TABLE IF NOT EXISTS import_state (
            file_path TEXT PRIMARY KEY,
            conversation_id TEXT,
            chunks_imported INTEGER,
            imported_at TEXT DEFAULT (datetime('now')),
            file_mtime TEXT,
            csr_tool_blocks_suppressed INTEGER NOT NULL DEFAULT 0,
            csr_hook_wrappers_scrubbed INTEGER NOT NULL DEFAULT 0,
            secrets_redacted TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_import_conversation_id
            ON import_state(conversation_id);
        ",
    )?;

    super::embedding_changes::migrate(conn)?;

    // Enrichment state — tracks per-conversation progressive enrichment
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS enrichment_state (
            conversation_id TEXT NOT NULL,
            enrichment_type TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'pending',
            reflection_id TEXT,
            batch_id TEXT,
            error_message TEXT,
            file_path TEXT,
            chunk_count INTEGER,
            prompt_hash TEXT,
            created_at TEXT DEFAULT (datetime('now')),
            updated_at TEXT DEFAULT (datetime('now')),
            PRIMARY KEY (conversation_id, enrichment_type)
        );

        CREATE INDEX IF NOT EXISTS idx_enrichment_status
            ON enrichment_state(enrichment_type, status);
        ",
    )?;

    if !has_column(conn, "enrichment_state", "batch_failures")? {
        add_column(
            conn,
            "enrichment_state",
            "batch_failures",
            "ALTER TABLE enrichment_state ADD COLUMN batch_failures INTEGER NOT NULL DEFAULT 0;",
        )?;
    }

    if !has_column(conn, "enrichment_state", "batch_transient_failures")? {
        add_column(conn, "enrichment_state", "batch_transient_failures",
            "ALTER TABLE enrichment_state ADD COLUMN batch_transient_failures INTEGER NOT NULL DEFAULT 0")?;
    }

    // The newest transcript line a session story was generated from. Nullable: stories written
    // before it was kept stay as they are and fall back to the reflection's write time.
    if !has_column(conn, "enrichment_state", "told_through")? {
        add_column(
            conn,
            "enrichment_state",
            "told_through",
            "ALTER TABLE enrichment_state ADD COLUMN told_through TEXT",
        )?;
    }

    // TAD: retrieval events — tracks which memories were surfaced and session outcomes
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS retrieval_events (
            id TEXT PRIMARY KEY,
            memory_id TEXT NOT NULL,
            memory_type TEXT NOT NULL,
            retrieved_at TEXT NOT NULL,
            hook_phase TEXT NOT NULL,
            session_outcome TEXT DEFAULT 'neutral',
            session_id TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_retrieval_memory ON retrieval_events(memory_id);
        CREATE INDEX IF NOT EXISTS idx_retrieval_session ON retrieval_events(session_id);
        ",
    )?;

    // Outcome scoring: retrieval stats rollup (v9)
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS retrieval_stats (
            memory_id TEXT PRIMARY KEY,
            success_count INTEGER DEFAULT 0,
            failure_count INTEGER DEFAULT 0,
            neutral_count INTEGER DEFAULT 0,
            last_updated TEXT DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS idx_retrieval_stats_updated
            ON retrieval_stats(last_updated DESC);
        ",
    )?;

    // Code evolution tracking (v9)
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS code_evolution (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            project_name TEXT DEFAULT '',
            file_path TEXT NOT NULL,
            language TEXT DEFAULT '',
            timestamp TEXT NOT NULL DEFAULT (datetime('now')),
            tool_name TEXT DEFAULT '',
            functions_added TEXT DEFAULT '[]',
            functions_removed TEXT DEFAULT '[]',
            types_added TEXT DEFAULT '[]',
            types_removed TEXT DEFAULT '[]',
            imports_added TEXT DEFAULT '[]',
            imports_removed TEXT DEFAULT '[]'
        );

        CREATE INDEX IF NOT EXISTS idx_code_evolution_file ON code_evolution(file_path);
        CREATE INDEX IF NOT EXISTS idx_code_evolution_session ON code_evolution(session_id, timestamp);

        CREATE TABLE IF NOT EXISTS episode_anchors (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            project    TEXT NOT NULL,
            file       TEXT NOT NULL,
            node_kind  TEXT NOT NULL,
            name       TEXT NOT NULL,
            body_hash  TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(session_id, file, name)
        );
        CREATE INDEX IF NOT EXISTS idx_episode_anchors_project ON episode_anchors(project);
        CREATE INDEX IF NOT EXISTS idx_episode_anchors_name ON episode_anchors(name);

        CREATE TABLE IF NOT EXISTS chunk_provenance (
            chunk_id       TEXT PRIMARY KEY REFERENCES chunks(id),
            author         TEXT NOT NULL,
            source_conv_id TEXT NOT NULL,
            supersedes     TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_chunk_provenance_author ON chunk_provenance(author);

        -- W3-B1 review#2 (HIGH): the transcript's own `entrypoint` field
        -- (`cli`, `sdk-cli`, `sdk-*`, `claude-desktop`, ...), recorded once
        -- per conversation at import time. Replaces
        -- `session_registry`-absence as the headless signal for
        -- `queries::filter_unregistered_conversation_rows` — registry
        -- absence only ever meant \"never typed a prompt through
        -- history.jsonl\", which is also true of a real interactive session
        -- history.jsonl never recorded. Missing here (never populated, or
        -- pre-dates this column) means \"unknown\", not \"headless\".
        CREATE TABLE IF NOT EXISTS conversation_entrypoint (
            conversation_id TEXT PRIMARY KEY,
            entrypoint      TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS derivation_ledger (
            id           TEXT NOT NULL,
            content      TEXT NOT NULL,
            anchor       TEXT,
            cost_bucket  TEXT NOT NULL,
            inferability REAL NOT NULL,
            confidence   REAL NOT NULL,
            times_reused INTEGER NOT NULL DEFAULT 0,
            repo         TEXT NOT NULL,
            branch       TEXT NOT NULL,
            user         TEXT NOT NULL,
            created_at   TEXT NOT NULL DEFAULT (datetime('now')),
            -- Facts are scoped {repo,branch,user}; the same fact id in a different
            -- scope is a distinct row, never a clobber (Codex HIGH).
            PRIMARY KEY (id, repo, branch, user)
        );
        CREATE INDEX IF NOT EXISTS idx_ledger_scope ON derivation_ledger(repo, branch, user);
        ",
    )?;

    // Code property graph (v9.4) — conversation-provenance code graph.
    // Additive + non-breaking: episode_anchors keeps writing; the graph reads code_nodes.
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS code_nodes (
            id          TEXT PRIMARY KEY,          -- sha256(repo|file|kind|name), truncated to 40 hex chars
            repo        TEXT NOT NULL DEFAULT '',
            project     TEXT NOT NULL DEFAULT '',
            file        TEXT NOT NULL,
            lang        TEXT NOT NULL DEFAULT '',
            kind        TEXT NOT NULL,             -- function|type|method|import|module
            name        TEXT NOT NULL,
            fqname      TEXT NOT NULL DEFAULT '',
            body_hash   TEXT NOT NULL DEFAULT '',
            span_start  INTEGER NOT NULL DEFAULT 0,
            span_end    INTEGER NOT NULL DEFAULT 0,
            first_conv_id   TEXT NOT NULL DEFAULT '',
            last_conv_id    TEXT NOT NULL DEFAULT '',
            last_session_id TEXT NOT NULL DEFAULT '',
            last_seen   TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_code_nodes_name ON code_nodes(name);
        CREATE INDEX IF NOT EXISTS idx_code_nodes_file ON code_nodes(project, file);
        CREATE INDEX IF NOT EXISTS idx_code_nodes_repo ON code_nodes(repo);

        CREATE TABLE IF NOT EXISTS code_edges (
            src_id      TEXT NOT NULL,
            dst_id      TEXT NOT NULL,
            kind        TEXT NOT NULL,             -- calls|imports|references|defines|implements|supersedes
            src_file    TEXT NOT NULL DEFAULT '',  -- file the edge was extracted from (per-file replace)
            resolved    INTEGER NOT NULL DEFAULT 0,
            weight      REAL NOT NULL DEFAULT 1.0,
            conv_id     TEXT NOT NULL DEFAULT '',
            session_id  TEXT NOT NULL DEFAULT '',
            created_at  TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (src_id, dst_id, kind)
        );
        CREATE INDEX IF NOT EXISTS idx_code_edges_src  ON code_edges(src_id, kind);
        CREATE INDEX IF NOT EXISTS idx_code_edges_dst  ON code_edges(dst_id, kind);
        -- idx_code_edges_file created after the src_file ALTER guard below (old tables lack the column).

        -- Per-file extraction state — drives dirty-flag + lazy recompute (Codex #3).
        CREATE TABLE IF NOT EXISTS code_graph_file_state (
            project      TEXT NOT NULL,
            file         TEXT NOT NULL,
            mtime        TEXT NOT NULL DEFAULT '',
            content_hash TEXT NOT NULL DEFAULT '',
            dirty        INTEGER NOT NULL DEFAULT 1,
            extracted_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (project, file)
        );
        CREATE INDEX IF NOT EXISTS idx_code_graph_file_dirty ON code_graph_file_state(dirty);

        CREATE TABLE IF NOT EXISTS code_node_rank (
            node_id     TEXT PRIMARY KEY REFERENCES code_nodes(id),
            rank        REAL NOT NULL DEFAULT 0.0,
            in_degree   INTEGER NOT NULL DEFAULT 0,
            out_degree  INTEGER NOT NULL DEFAULT 0,
            computed_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_code_node_rank_rank ON code_node_rank(rank DESC);
        ",
    )?;

    // Migration: add src_file to code_edges if missing (v9.4 — table may predate the column)
    {
        let has_src_file: bool = conn
            .prepare("SELECT src_file FROM code_edges LIMIT 0")
            .is_ok();
        if !has_src_file {
            add_column(
                conn,
                "code_edges",
                "src_file",
                "ALTER TABLE code_edges ADD COLUMN src_file TEXT NOT NULL DEFAULT '';",
            )?;
        }
        let _ = conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_code_edges_file ON code_edges(src_file);",
        );
    }

    // Migration: witness-closure resolution columns on code_edges (v9.5 Phase 1).
    // callee_kind is captured at extraction time (before bare_callee() strips the
    // receiver); boundary + evidence are populated later by the resolver (Phase 2+).
    // Same idempotent ALTER-guard pattern as the src_file migration above.
    {
        let has_callee_kind: bool = conn
            .prepare("SELECT callee_kind FROM code_edges LIMIT 0")
            .is_ok();
        if !has_callee_kind {
            add_column(
                conn,
                "code_edges",
                "callee_kind",
                "ALTER TABLE code_edges ADD COLUMN callee_kind TEXT DEFAULT '';",
            )?;
        }
        let has_boundary: bool = conn
            .prepare("SELECT boundary FROM code_edges LIMIT 0")
            .is_ok();
        if !has_boundary {
            add_column(
                conn,
                "code_edges",
                "boundary",
                "ALTER TABLE code_edges ADD COLUMN boundary TEXT DEFAULT '';",
            )?;
        }
        let has_evidence: bool = conn
            .prepare("SELECT evidence FROM code_edges LIMIT 0")
            .is_ok();
        if !has_evidence {
            add_column(
                conn,
                "code_edges",
                "evidence",
                "ALTER TABLE code_edges ADD COLUMN evidence TEXT DEFAULT '';",
            )?;
        }
    }

    // Migration: src_content_hash on code_edges (WCR truth pass, Codex round 7
    // adversarial review). Immutable per-edge write-time provenance: the
    // whole-file content hash (`extraction::codegraph::body_hash` of the SAME
    // source that produced this edge), stamped once by `extract_inner`'s
    // `add_edge` closure and never independently refreshed — unlike
    // `code_nodes.body_hash`, which `upsert_node` refreshes on every sighting
    // in a SEPARATE transaction from the edge replace. That mutability +
    // transaction split is exactly what let a partial write (nodes refreshed,
    // edge replace failed or simply never ran) falsely authenticate a stale
    // edge for re-pointing — see
    // `eval::codegraph::historical_src_content_unchanged`'s doc comment for
    // the full finding. DEFAULT '' means "absent" (a legacy edge written
    // before this column existed, or by a path that hasn't re-extracted
    // since) — categorically ineligible for re-pointing, never a guess.
    {
        let has_src_content_hash: bool = conn
            .prepare("SELECT src_content_hash FROM code_edges LIMIT 0")
            .is_ok();
        if !has_src_content_hash {
            add_column(
                conn,
                "code_edges",
                "src_content_hash",
                "ALTER TABLE code_edges ADD COLUMN src_content_hash TEXT DEFAULT '';",
            )?;
        }
    }

    // repo_defs (v9.5 Phase 1): per-file symbol inventory (name/kind/lang) feeding
    // witness-closure resolution (Phase 2+). Populated by a repo scan; replace-per-file
    // semantics mirror code_edges (see upsert_repo_defs in storage/codegraph.rs).
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS repo_defs (
            project    TEXT NOT NULL,
            file       TEXT NOT NULL,
            name       TEXT NOT NULL,
            kind       TEXT NOT NULL,
            lang       TEXT NOT NULL DEFAULT '',
            scanned_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (project, file, name, kind)
        );",
    )?;

    // Migration: repo_root identity column on code_nodes + code_evolution
    // (WP2 Stage 1, H8 finding — receipt R4). `project` is the session-cwd
    // tag (its own signal, never overwritten here); `repo_root` is the git
    // toplevel of the row's file at index/backfill time
    // (`extraction::repo_root::repo_root_for_file`) — a stable identity that
    // collapses two different session cwds checked out inside the SAME
    // repository (e.g. `claude-self-reflect` and its `csr-engine`
    // subdirectory opened as its own session) onto one label. Nullable:
    // non-git files, or files whose repo can no longer be resolved (git
    // missing, no `.git` found walking up), get NULL — never a guess.
    {
        let has_repo_root_nodes: bool = conn
            .prepare("SELECT repo_root FROM code_nodes LIMIT 0")
            .is_ok();
        if !has_repo_root_nodes {
            add_column(
                conn,
                "code_nodes",
                "repo_root",
                "ALTER TABLE code_nodes ADD COLUMN repo_root TEXT;",
            )?;
        }
        let has_last_chunk_id: bool = conn
            .prepare("SELECT last_chunk_id FROM code_nodes LIMIT 0")
            .is_ok();
        if !has_last_chunk_id {
            add_column(
                conn,
                "code_nodes",
                "last_chunk_id",
                "ALTER TABLE code_nodes ADD COLUMN last_chunk_id TEXT;",
            )?;
        }
        let _ = conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_code_nodes_repo_root ON code_nodes(repo_root);",
        );

        let has_repo_root_evolution: bool = conn
            .prepare("SELECT repo_root FROM code_evolution LIMIT 0")
            .is_ok();
        if !has_repo_root_evolution {
            add_column(
                conn,
                "code_evolution",
                "repo_root",
                "ALTER TABLE code_evolution ADD COLUMN repo_root TEXT;",
            )?;
        }
        let _ = conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_code_evolution_repo_root ON code_evolution(repo_root);",
        );
    }

    // code_node_attribution (WP2 Stage 2, H4/H5/H6 remediation — receipts
    // R2/R3/R8 in `.plans/2026-07-31-codegraph-shipping-plan.md`). Two
    // independent, NEVER-merged provenance channels per symbol:
    // `transcript` (the agent conversation whose `code_evolution` event
    // first named the symbol) and `git` (the commit that introduced the
    // symbol's current line span, via `git log -L … --reverse`). Replaces
    // `code_nodes.first_conv_id` as the thing consumer surfaces present as
    // "introduction evidence" — H4 measured `first_conv_id` at 50.7%
    // agreement with the evidence-bearing join (file-level projection, not a
    // real per-symbol fact); `first_conv_id` itself stays in the schema for
    // compat, just no longer trusted for this purpose.
    //
    // `channel` CHECK constraint keeps the table from ever silently
    // accepting a third pseudo-channel. `source_id` is a session id
    // (transcript) or a commit hash (git); `observed_ts` is the event/commit
    // timestamp, nullable because a channel can be written before a
    // timestamp is known; `evidence` is a short machine tag
    // (`coedit_event` | `git_log_L`) naming the derivation, not free text.
    // PRIMARY KEY(node_id, channel) — at most one row per node per channel,
    // so `code_node_attribution::upsert_attribution` is a pure idempotent
    // replace, safe to re-run the backfill.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS code_node_attribution (
            node_id     TEXT NOT NULL,
            channel     TEXT NOT NULL CHECK(channel IN ('transcript','git')),
            source_id   TEXT NOT NULL,
            observed_ts TEXT,
            evidence    TEXT,
            PRIMARY KEY(node_id, channel)
        );
        CREATE INDEX IF NOT EXISTS idx_code_node_attribution_node
            ON code_node_attribution(node_id);",
    )?;

    // TAD v2 release-ancestry cache. Only conversations backed by the exact
    // transcript -> node -> git attribution join are stored here. The daemon
    // replaces the cache atomically after walking release ancestry; retrieval
    // performs only an indexed conversation-id lookup and never invokes git.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS conversation_ancestry_cache (
            conversation_id TEXT PRIMARY KEY,
            state           TEXT NOT NULL CHECK(state IN ('shipped','unreleased')),
            release_tag     TEXT,
            releases_behind INTEGER NOT NULL CHECK(releases_behind >= 0),
            repository      TEXT NOT NULL,
            refreshed_at    TEXT NOT NULL,
            CHECK(
                (state = 'shipped' AND release_tag IS NOT NULL)
                OR
                (state = 'unreleased' AND release_tag IS NULL AND releases_behind = 0)
            )
        );",
    )?;

    // Migration: ast_status column on code_graph_file_state (WP2 Stage 3, H8
    // innovation — receipt R4 in
    // `.plans/2026-07-31-codegraph-shipping-plan.md`). File-level provenance
    // for files the extraction write paths (hook `update_code_graph`,
    // `import::backfill`) SAW but could not parse because the extension is
    // outside the six supported languages: instead of silently dropping the
    // file (the pre-existing behavior), a row is written with
    // `ast_status='unsupported'` so the file stays traversable at
    // file-granularity rather than vanishing. Default `'supported'` — every
    // pre-existing row (and every row `upsert_file_state` still writes after
    // a real extraction) is, by construction, a file CSR actually parsed.
    {
        let has_ast_status: bool = conn
            .prepare("SELECT ast_status FROM code_graph_file_state LIMIT 0")
            .is_ok();
        if !has_ast_status {
            add_column(conn, "code_graph_file_state", "ast_status", "ALTER TABLE code_graph_file_state ADD COLUMN ast_status TEXT NOT NULL DEFAULT 'supported';")?;
        }
        let _ = conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_code_graph_file_state_ast_status ON code_graph_file_state(ast_status);",
        );
    }

    // Migration: per-file resolve generations on code_graph_file_state. An
    // extraction bumps `graph_gen`; the drain (`extraction::graph_drain`) runs
    // the project-wide resolve and then sets `resolved_gen` to the generation it
    // read before it started, only if the file has not moved on since. A file
    // is pending while `graph_gen > resolved_gen`. Both start at 0, so no
    // existing row is pending and the upgrade queues no work.
    {
        if !has_column(conn, "code_graph_file_state", "graph_gen")? {
            add_column(conn, "code_graph_file_state", "graph_gen", "ALTER TABLE code_graph_file_state ADD COLUMN graph_gen INTEGER NOT NULL DEFAULT 0;")?;
        }
        if !has_column(conn, "code_graph_file_state", "resolved_gen")? {
            add_column(conn, "code_graph_file_state", "resolved_gen", "ALTER TABLE code_graph_file_state ADD COLUMN resolved_gen INTEGER NOT NULL DEFAULT 0;")?;
        }
        let _ = conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_code_graph_file_state_pending
                 ON code_graph_file_state(project) WHERE graph_gen > resolved_gen;",
        );
        // A build that resolved only the edited file left the rest of the work
        // as `dirty = 1` plus a `graph_catchup:*` hint in `meta`, for a catch-up
        // that no longer exists. Queue those files for the drain and drop the
        // hints. A file the drain has settled is no longer at 0/0, so this does
        // not requeue it on the next start.
        // Reads first: an up-to-date database must migrate without taking the
        // write lock.
        let stranded: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM code_graph_file_state
                 WHERE dirty = 1 AND graph_gen = 0 AND resolved_gen = 0)",
            [],
            |r| r.get(0),
        )?;
        if stranded {
            conn.execute(
                "UPDATE code_graph_file_state SET graph_gen = 1, dirty = 0
                 WHERE dirty = 1 AND graph_gen = 0 AND resolved_gen = 0",
                [],
            )?;
        }
        let has_meta: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'meta')",
            [],
            |r| r.get(0),
        )?;
        if has_meta {
            let hints: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM meta WHERE key LIKE 'graph_catchup:%')",
                [],
                |r| r.get(0),
            )?;
            if hints {
                conn.execute("DELETE FROM meta WHERE key LIKE 'graph_catchup:%'", [])?;
            }
        }
    }

    // Migration: one counter per project, bumped by every write a resolve reads
    // (node upsert/retire, edge replace, repo_defs replace, a file's generation
    // bump). The drain plans a resolve without the write lock and re-reads this
    // counter under it: if it moved, the plan is stale and is discarded.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS code_graph_project_gen (
             project TEXT PRIMARY KEY,
             gen     INTEGER NOT NULL DEFAULT 0
         );",
    )?;

    // Migration: the counter values a completed resolve pass covered, per
    // project (`own` is the project's row, `star` the every-project row). The
    // drain treats a project whose counter is ahead of this as work even with
    // no file pending, so a write the resolve reads that queues no file (a repo
    // scan, a co-edit replay) still gets its pass.
    //
    // A table created now starts EMPTY (nothing covered) and every project that
    // has graph content gets its counter bumped, so each existing project is
    // resolved exactly once after the upgrade. Seeding it from the counters as
    // they stand would declare settled whatever an older drain left uncovered
    // (a repo scan that bumped a generation and queued no file), and nothing
    // would revisit it until another write.
    create_settled_table(conn)?;

    // Migration: the edits a PostToolUse hook snapshots for the drain. The hook
    // does not extract: it records what it saw (the file's content, the
    // conversation / session / transcript chunk that edited it) and the drain
    // extracts the snapshots in `seq` order, one at a time, each with its own
    // attribution. `seq` is the capture time in unix nanoseconds, so the DB
    // path and the lock-free spool (`extraction::graph_spool`) order on one
    // clock. `content` NULL is a legacy queue row: the drain reads `read_path`.
    // `origin` is the spool file a row came from (replay guard).
    // `code_graph_snap_hw` is the highest `seq` the drain has consumed per
    // file: a snapshot at or below it arrived late and is dropped, never
    // allowed to put an older version over a newer one.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS code_graph_snapshots (
             seq             INTEGER PRIMARY KEY,
             project         TEXT NOT NULL,
             file            TEXT NOT NULL,
             read_path       TEXT NOT NULL,
             kind            TEXT NOT NULL DEFAULT 'extract',
             content         BLOB,
             conv_id         TEXT NOT NULL DEFAULT '',
             session_id      TEXT NOT NULL DEFAULT '',
             source_chunk_id TEXT,
             origin          TEXT
         );
         CREATE UNIQUE INDEX IF NOT EXISTS idx_code_graph_snapshots_origin
             ON code_graph_snapshots(origin) WHERE origin IS NOT NULL;
         CREATE INDEX IF NOT EXISTS idx_code_graph_snapshots_file
             ON code_graph_snapshots(project, file, seq);
         CREATE TABLE IF NOT EXISTS code_graph_snap_hw (
             project TEXT NOT NULL,
             file    TEXT NOT NULL,
             seq     INTEGER NOT NULL,
             PRIMARY KEY (project, file)
         );",
    )?;
    // The queue this replaces (one row per file; its `seq` was that file's
    // `graph_gen`, not a global order). Its rows become snapshots with no
    // content, numbered 1..n in (old seq, project, file) order, below any
    // capture time. Reads first: an up-to-date database takes no write lock.
    let legacy_queue: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name = 'code_graph_extract_queue')",
        [],
        |r| r.get(0),
    )?;
    if legacy_queue {
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO code_graph_snapshots
                 (project, file, read_path, kind, conv_id, session_id)
             SELECT project, file, read_path, 'extract', conv_id, session_id
             FROM code_graph_extract_queue ORDER BY seq, project, file",
            [],
        )?;
        tx.execute("DROP TABLE code_graph_extract_queue", [])?;
        tx.commit()?;
    }

    // Migration: add project_name to code_evolution if missing (v9 cross-project fix)
    {
        let has_project_col: bool = conn
            .prepare("SELECT project_name FROM code_evolution LIMIT 0")
            .is_ok();
        if !has_project_col {
            add_column(
                conn,
                "code_evolution",
                "project_name",
                "ALTER TABLE code_evolution ADD COLUMN project_name TEXT DEFAULT '';",
            )?;
        }
        // Always try to create the index (safe with IF NOT EXISTS)
        let _ = conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_code_evolution_project ON code_evolution(project_name, file_path);",
        );
    }

    // Migration: add conversation_id to import_state if missing (for existing DBs)
    let has_conv_col: bool = conn
        .prepare("SELECT conversation_id FROM import_state LIMIT 0")
        .is_ok();
    if !has_conv_col {
        add_column(
            conn,
            "import_state",
            "conversation_id",
            "ALTER TABLE import_state ADD COLUMN conversation_id TEXT",
        )?;
        conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_import_conversation_id ON import_state(conversation_id);")?;
    }

    // Per-file cumulative suppression totals make the global counters idempotent
    // across the full-file reparses used by incremental transcript imports.
    // Legacy rows stay NULL until their first reparse establishes a baseline;
    // fresh databases use the NOT NULL zero defaults in CREATE TABLE above.
    let has_tool_suppression_col = conn
        .prepare("SELECT csr_tool_blocks_suppressed FROM import_state LIMIT 0")
        .is_ok();
    if !has_tool_suppression_col {
        add_column(
            conn,
            "import_state",
            "csr_tool_blocks_suppressed",
            "ALTER TABLE import_state ADD COLUMN csr_tool_blocks_suppressed INTEGER;",
        )?;
    }
    let has_wrapper_suppression_col = conn
        .prepare("SELECT csr_hook_wrappers_scrubbed FROM import_state LIMIT 0")
        .is_ok();
    if !has_wrapper_suppression_col {
        add_column(
            conn,
            "import_state",
            "csr_hook_wrappers_scrubbed",
            "ALTER TABLE import_state ADD COLUMN csr_hook_wrappers_scrubbed INTEGER;",
        )?;
    }

    // Per-file cumulative secret-redaction counts (JSON `{kind: n}`, NULL = none),
    // for the same reason as the suppression totals above: a reparse of a growing
    // transcript sees the same secrets again and must not count them twice.
    let has_secrets_col = conn
        .prepare("SELECT secrets_redacted FROM import_state LIMIT 0")
        .is_ok();
    if !has_secrets_col {
        add_column(
            conn,
            "import_state",
            "secrets_redacted",
            "ALTER TABLE import_state ADD COLUMN secrets_redacted TEXT;",
        )?;
    }

    // Migration: add summary column to chunks table if missing (for timeline display)
    let has_summary_col: bool = conn.prepare("SELECT summary FROM chunks LIMIT 0").is_ok();
    if !has_summary_col {
        add_column(
            conn,
            "chunks",
            "summary",
            "ALTER TABLE chunks ADD COLUMN summary TEXT;",
        )?;
    }

    // Migration: add seq + is_sidechain columns to chunks table (Saga Phase 1 WS1).
    // seq is nullable — old rows stay NULL until backfilled; is_sidechain defaults 0
    // (never sidechain) until backfilled. Both additive, no table rebuild.
    let has_seq_col: bool = conn.prepare("SELECT seq FROM chunks LIMIT 0").is_ok();
    if !has_seq_col {
        add_column(
            conn,
            "chunks",
            "seq",
            "ALTER TABLE chunks ADD COLUMN seq INTEGER;",
        )?;
    }
    let has_sidechain_col: bool = conn
        .prepare("SELECT is_sidechain FROM chunks LIMIT 0")
        .is_ok();
    if !has_sidechain_col {
        add_column(
            conn,
            "chunks",
            "is_sidechain",
            "ALTER TABLE chunks ADD COLUMN is_sidechain INTEGER NOT NULL DEFAULT 0;",
        )?;
    }

    // Migration: chunk source dimension (v9.4 multi-source corpus). Pure additive
    // ALTER, O(1) on multi-GB DBs. Deliberately NO index: two-value column with no
    // consuming query yet (Codex adversarial review — a CREATE INDEX here would scan
    // the full chunks table during Storage::open on production DBs).
    let has_source_col: bool = conn.prepare("SELECT source FROM chunks LIMIT 0").is_ok();
    if !has_source_col {
        add_column(
            conn,
            "chunks",
            "source",
            "ALTER TABLE chunks ADD COLUMN source TEXT NOT NULL DEFAULT 'conversation';",
        )?;
    }

    migrate_provenance_substrate(conn)?;

    // The FTS migration records its durable compaction state in meta. Create
    // this small table before the larger metadata block below so the schema
    // replacement and its pending marker can commit atomically.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    )?;
    migrate_chunks_fts(conn)?;
    migrate_chunks_fts_au_content_scoped(conn)?;
    migrate_episodes(conn)?;
    super::assertions::migrate_assertions(conn)?;

    // Engine metadata KV — caches expensive computed state (e.g. integrity_check
    // results, which cost ~10s on multi-GB DBs and must not run per status call).
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS narrative_usage (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ts TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
            call_site TEXT NOT NULL,
            model TEXT NOT NULL,
            input_tokens INTEGER NOT NULL DEFAULT 0,
            output_tokens INTEGER NOT NULL DEFAULT 0,
            cache_read_tokens INTEGER NOT NULL DEFAULT 0,
            cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
            duration_ms INTEGER NOT NULL DEFAULT 0,
            success INTEGER NOT NULL DEFAULT 1
         );
         CREATE TABLE IF NOT EXISTS journal_headlines (
            session_id TEXT PRIMARY KEY,
            content_hash TEXT NOT NULL,
            headline TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            model TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
         );
         CREATE TABLE IF NOT EXISTS ratification_scores (
            conversation_id TEXT PRIMARY KEY,
            score REAL NOT NULL,
            acts_json TEXT NOT NULL,
            ledger_refs TEXT,
            extractor_version TEXT NOT NULL,
            extracted_at INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS session_instrumentation (
             session_id       TEXT PRIMARY KEY,
             transcript_size  INTEGER NOT NULL DEFAULT 0,
             transcript_mtime INTEGER NOT NULL DEFAULT 0,
             error_count      INTEGER NOT NULL DEFAULT 0,
             steer_count      INTEGER NOT NULL DEFAULT 0,
             turn_count       INTEGER NOT NULL DEFAULT 0,
             errors_json      TEXT NOT NULL DEFAULT '[]',
             steers_json      TEXT NOT NULL DEFAULT '[]',
             computed_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
         );",
    )?;

    // One-time cache invalidation (steer-noise fix, `transcript::instrumentation::
    // is_noisy_steer_text`): rows cached under the pre-fix steer filter may carry
    // harness noise (`<task-notification>` blocks, `[SYSTEM NOTIFICATION` wrappers)
    // that no human ever typed. `run()` executes on every `Storage::open`, so an
    // unconditional DELETE here would wipe the cache on every process start instead
    // of once — guarded by `meta` so it fires exactly once per database. The table
    // self-heals: `dream::report`'s backfill refills it in ~1.3s at the next report.
    {
        let already_purged =
            crate::storage::queries::get_meta(conn, "steer_noise_filter_v1")?.is_some();
        if !already_purged {
            conn.execute_batch("DELETE FROM session_instrumentation;")?;
            crate::storage::queries::set_meta(conn, "steer_noise_filter_v1", "1")?;
        }
    }

    // Second purge, same pattern: the steer filter changed again after v1
    // (queued-prefix normalization, F3 of the certification review), so rows
    // cached between the two fixes may hold `[queued] [SYSTEM NOTIFICATION`
    // noise. One wipe makes every surviving cache row a product of the
    // current filter generation — which is what lets the renderer trust
    // cache-sourced steer totals without a per-row version column.
    {
        let already_purged =
            crate::storage::queries::get_meta(conn, "steer_noise_filter_v2")?.is_some();
        if !already_purged {
            conn.execute_batch("DELETE FROM session_instrumentation;")?;
            crate::storage::queries::set_meta(conn, "steer_noise_filter_v2", "1")?;
        }
    }

    // Keyword index over reflections (see `CREATE_REFLECTIONS_FTS`). One-shot
    // per database: create the table and triggers, then backfill every row
    // that already exists; the triggers keep it current from then on.
    {
        let installed = crate::storage::queries::get_meta(conn, "reflections_fts_v1")?.is_some();
        if !installed {
            conn.execute_batch(CREATE_REFLECTIONS_FTS)?;
            conn.execute_batch(
                "INSERT INTO reflections_fts(id, content) SELECT id, content FROM reflections;",
            )?;
            crate::storage::queries::set_meta(conn, "reflections_fts_v1", "1")?;
        }
    }

    // Migration: journal_headlines gained `description` after first ship on
    // 2026-08-10; DBs created between the two shapes lack the column. Same
    // idempotent ALTER-guard pattern as the code_edges columns above.
    {
        let has_description: bool = conn
            .prepare("SELECT description FROM journal_headlines LIMIT 0")
            .is_ok();
        if !has_description {
            add_column(
                conn,
                "journal_headlines",
                "description",
                "ALTER TABLE journal_headlines ADD COLUMN description TEXT NOT NULL DEFAULT '';",
            )?;
        }
    }

    // Migration: resolution ledger (v9.4+) — append-only verdicts per chunk_id;
    // latest row wins on read. No FK on chunk_id (may reference reflections too).
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS resolution_ledger (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            chunk_id TEXT NOT NULL,
            status TEXT NOT NULL CHECK(status IN ('resolved','still_open','regressed')),
            evidence TEXT NOT NULL,
            claim TEXT,
            source TEXT NOT NULL DEFAULT 'agent'
                CHECK(source IN ('agent','user_confirmed')),
            created_at TEXT DEFAULT (datetime('now'))
         );
         CREATE INDEX IF NOT EXISTS idx_resolution_chunk ON resolution_ledger(chunk_id, id);
         CREATE TRIGGER IF NOT EXISTS resolution_source_insert_guard
         BEFORE INSERT ON resolution_ledger
         WHEN NEW.source NOT IN ('agent','user_confirmed')
         BEGIN
             SELECT RAISE(ABORT, 'invalid resolution source');
         END;
         CREATE TRIGGER IF NOT EXISTS resolution_source_immutable_guard
         BEFORE UPDATE OF source ON resolution_ledger
         WHEN NEW.source <> OLD.source
         BEGIN
             SELECT RAISE(ABORT, 'resolution source is immutable');
         END;",
    )?;

    // v9.4 multi-source corpus: session registry (history.jsonl spine — never embedded,
    // never injected) and resolution proposals (task-derived candidates; invisible to
    // search/annotation until a human promotes them via csr_resolve — Codex adversarial
    // review: automatic writes to resolution_ledger would be indistinguishable from
    // human verdicts at read time).
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS session_registry (
            session_id TEXT PRIMARY KEY,
            project TEXT NOT NULL,
            first_prompt TEXT,
            first_ts TEXT,
            last_ts TEXT,
            prompt_count INTEGER NOT NULL DEFAULT 0
         );
         CREATE INDEX IF NOT EXISTS idx_session_registry_project ON session_registry(project);
         CREATE TABLE IF NOT EXISTS resolution_proposals (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            chunk_id TEXT NOT NULL,
            claim TEXT,
            evidence TEXT NOT NULL,
            session_id TEXT NOT NULL,
            created_at TEXT DEFAULT (datetime('now')),
            UNIQUE(chunk_id, session_id)
         );",
    )?;

    // local_bindings (WCR truth pass, TASK 2; scope-qualified by the X4
    // adversarial-review truth pass, Finding 1; CHAIN-qualified by the
    // second X4 adversarial review, Finding 4): the X4 `local` classify
    // tier's witness table — per (project, file, scope), the set of names
    // bound as function/closure parameters or local (non-top-level) variable
    // declarations, as gathered by
    // `extraction::codegraph::collect_local_bindings`. `scope` is the FULL
    // scope CHAIN of the nearest enclosing NAMED definition the binding sits
    // inside (see `extraction::codegraph::scope_chain`'s doc comment — e.g.
    // `"Component"`, `"Component>anon12"`), or '' for module-level bindings
    // — without it, a single flat name-set per (project, file) let a
    // parameter named `handler` in one function OR ONE ANONYMOUS CLOSURE
    // classify an unrelated sibling scope's own `handler()` call as local.
    // Populated by `eval::codegraph::backfill_wcr_witnesses` from the SAME
    // parse it already does for callee_kind/evidence backfill — never a
    // second parse. Read by `extraction::resolver::resolve_edges`'s X4 tier
    // (prefix-matched against each pending edge's own chain — see
    // `edge_scope_chains`, below).
    //
    // DROP+CREATE (Finding 1, X4 adversarial review): this table only ever
    // holds shadow/witness data, rebuilt from scratch by every
    // `backfill_wcr_witnesses` pass (see `persist_local_bindings`'s
    // transactional replace-per-file semantics — Finding 2), so an
    // a shape-probed drop-and-recreate is needed — a pre-existing DB with
    // the OLD 3-column (project, file, name) schema would otherwise silently
    // keep that schema forever (`CREATE TABLE IF NOT EXISTS` is a no-op once
    // the table exists), breaking every query below that now expects `scope`. The
    // `scope` column's CONTENT changed from a bare enclosing-def name to a
    // full chain string (Finding 4) without any column/shape change, so no
    // further migration bump was needed for that step.
    // Shape-probed drop (CodeRabbit PR #279): `run` executes on EVERY
    // `Storage::open`, so an unconditional drop here is not a one-time schema
    // upgrade — it wiped the witness rows persisted by the last
    // `backfill_wcr_witnesses` pass on every subsequent hook/MCP process
    // start, silently disabling the X4 `local` tier in live use. Drop only
    // when the table exists in the pre-`scope` legacy shape.
    let lb_has_scope_in_pk: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('local_bindings')
         WHERE name = 'scope' AND pk > 0",
        [],
        |r| r.get(0),
    )?;
    if lb_has_scope_in_pk == 0 {
        conn.execute_batch("DROP TABLE IF EXISTS local_bindings;")?;
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS local_bindings (
            project TEXT NOT NULL,
            file    TEXT NOT NULL,
            scope   TEXT NOT NULL,
            name    TEXT NOT NULL,
            PRIMARY KEY (project, file, scope, name)
        );",
    )?;

    // edge_scope_chains (X4 adversarial review, Finding 4; multi-row-per-
    // edge as of the Codex round 4 adversarial review, Finding 1): per
    // pending `calls`/`imports` edge, the AST scope CHAIN of EVERY DISTINCT
    // physical call/import site `code_edges` aggregated into that one
    // `(src_id, dst_id, kind)` row. Before Finding 1, this table kept only
    // the FIRST site's chain per edge key (`INSERT OR IGNORE` against a PK
    // that didn't include `chain`), so an edge aggregating calls from two
    // DIFFERENT sibling closures silently discarded every occurrence but
    // one — order-dependent, and able to both falsely witness an aggregate
    // via an unrelated sibling closure's chain and hide a valid witness.
    // `chain` is now part of the PRIMARY KEY, so every distinct chain for
    // the same edge gets its own row (identical chains from repeat physical
    // sites still collapse to one row — `INSERT OR IGNORE` dedup, same as
    // before). `extraction::resolver::resolve_edges`'s X4 tier reads ALL
    // rows for an edge and requires a witness's binding chain to be a
    // prefix of EVERY one of them (conservative universal quantification —
    // see `Pending::call_scope_chains`' doc comment) rather than any single
    // one, matching the true "this edge only reduces to ONE call site"
    // semantics only when there genuinely is only one.
    //
    // Shape-probed DROP+CREATE (same reasoning as `local_bindings`,
    // immediately above): this table holds shadow/witness data rebuilt
    // per-file by `backfill_wcr_witnesses` passes (see
    // `persist_call_scope_chains`'s transactional replace-per-file
    // semantics); the drop is needed only for legacy shapes — a
    // pre-existing DB with the OLD
    // 5-column `(src_id, dst_id, kind)`-only-PK schema would otherwise
    // silently keep that schema forever (`CREATE TABLE IF NOT EXISTS` is a
    // no-op once the table exists), breaking every query below that now
    // expects `chain` to be part of the key (a stale single-row-per-edge DB
    // would silently drop every occurrence past the first on the next
    // `INSERT OR IGNORE`, reintroducing the exact bug this migration fixes).
    //
    // Populated by `eval::codegraph::backfill_wcr_witnesses` for EVERY
    // fresh `calls`/`imports` call/import site in a re-extracted file
    // (`extraction::codegraph::GraphFragment::call_scope_chains`),
    // unconditionally — same "persist everything this fresh pass saw"
    // philosophy as `local_bindings`, never filtered down to only the
    // edges some pending row happened to match. A pending edge that
    // MATCHES or gets RE-POINTED (Finding: attribution-skewed re-point,
    // see `backfill_wcr_witnesses`) to a fresh edge therefore ends this
    // backfill pass with a `(src_id, dst_id, kind)` that has at least one
    // row in this table; an edge left untouched (no fresh counterpart
    // found at all) has none — `extraction::resolver::resolve_edges`
    // fetches this table as a separate `(src_id, dst_id, kind) -> Vec<chain>`
    // map (never a JOIN against the main Pending query — a JOIN would fan
    // out into one Pending row per chain for a multi-occurrence edge,
    // corrupting the one-Pending-per-DB-row invariant every stat bucket
    // relies on) and falls back to the edge's own calling-def name when no
    // row exists (see `Pending::call_scope_chains`'s doc comment),
    // preserving the pre-chain exact-match behavior for edges this
    // backfill pass has no fresh chain data for.
    // Shape-probed drop, same reasoning as `local_bindings` above: the legacy
    // shape is detected by `chain` being absent from the PRIMARY KEY (the
    // pre-Finding-1 table either lacked the column or keyed only on
    // `(src_id, dst_id, kind)`); the current shape survives re-open with its
    // rows intact.
    let esc_has_chain_in_pk: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('edge_scope_chains')
         WHERE name = 'chain' AND pk > 0",
        [],
        |r| r.get(0),
    )?;
    if esc_has_chain_in_pk == 0 {
        conn.execute_batch("DROP TABLE IF EXISTS edge_scope_chains;")?;
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS edge_scope_chains (
            project TEXT NOT NULL,
            file    TEXT NOT NULL,
            src_id  TEXT NOT NULL,
            dst_id  TEXT NOT NULL,
            kind    TEXT NOT NULL,
            chain   TEXT NOT NULL,
            PRIMARY KEY (src_id, dst_id, kind, chain)
        );
        CREATE INDEX IF NOT EXISTS idx_edge_scope_chains_file ON edge_scope_chains(project, file);",
    )?;

    // witness_ledger (v10 "dreaming" substrate): append-only ledger of
    // `codewitness` content-hash stamps anchored to a file/symbol/span at a
    // specific git commit (or worktree). This is the evidence substrate for
    // future evidence-grounded forgetting — a claim gets a witness once, and
    // later audits (`codewitness::Auditor::try_audit`) check whether that
    // witness still holds, rather than re-deriving the claim from scratch or
    // trusting a wall-clock staleness heuristic.
    //
    // APPEND-ONLY INVARIANT: `storage::witness_ledger` exposes INSERT and
    // QUERY functions ONLY — there is no UPDATE or DELETE for this table. A
    // witness that no longer holds is superseded by inserting a NEW row (a
    // fresh stamp at a later commit), never by mutating or removing the old
    // one; see `storage::witness_ledger`'s module doc for the full rationale.
    //
    // Identity dedupe: `idx_witness_ledger_identity` (a UNIQUE expression
    // index, below) makes a repeat insert of an identical claim (e.g. an
    // idempotent `codegraph stamp-spans` re-run) conflict, and
    // `storage::witness_ledger::insert_witness` uses `INSERT OR IGNORE` so
    // the duplicate is a silent no-op. SQLite (like standard SQL) treats
    // every NULL in a plain UNIQUE constraint as distinct from every other
    // NULL, which is why this is an expression index over
    // `COALESCE(...)`-normalized key columns rather than an inline
    // `UNIQUE(...)` on the table: whole-file witnesses (`symbol`/
    // `span_start`/`span_end` all NULL) dedupe atomically at the DB level
    // too — see the
    // `witness_ledger_identity_index_dedupes_null_symbol_rows` test below.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS witness_ledger (
            id INTEGER PRIMARY KEY,
            project TEXT NOT NULL,
            file TEXT NOT NULL,
            symbol TEXT,
            span_start INTEGER, span_end INTEGER,
            stamp TEXT NOT NULL,
            tier TEXT NOT NULL CHECK (tier IN ('worktree','committed')),
            at_oid TEXT,
            source_kind TEXT NOT NULL,
            source_id TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_witness_ledger_identity ON witness_ledger (project, file, COALESCE(symbol,''), COALESCE(span_start,-1), COALESCE(span_end,-1), stamp, tier, COALESCE(at_oid,''), source_kind, COALESCE(source_id,''));
        CREATE INDEX IF NOT EXISTS idx_witness_ledger_lookup ON witness_ledger(project, file, symbol);
        CREATE INDEX IF NOT EXISTS idx_witness_ledger_file_symbol ON witness_ledger(file, symbol, at_oid);",
    )?;
    // `idx_witness_ledger_file_symbol` serves the project-agnostic anchor
    // probes (the HEAD-stamp / intact lookups and the correlated EXISTS in
    // the verdict-state readers): they match on (file, symbol[, at_oid])
    // with no project, and both indexes above lead with `project`, so
    // without it each probe scanned the whole ledger.

    // Mutable publication bookkeeping for append-only witness evidence.
    // `witness_ledger` itself remains immutable: a re-derivation run first
    // computes every row in memory, then one transaction inserts the rows
    // and a COMPLETE manifest. Failed runs may record an INCOMPLETE manifest
    // but publish no ledger rows. Binding therefore has an explicit atomic
    // publication boundary instead of inferring completeness from row order.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS witness_generations (
            id INTEGER PRIMARY KEY,
            generation_id TEXT NOT NULL UNIQUE,
            project TEXT NOT NULL,
            file TEXT NOT NULL,
            repo_root TEXT,
            head_oid TEXT NOT NULL,
            extractor_version TEXT NOT NULL,
            status TEXT NOT NULL CHECK (status IN ('complete','incomplete')),
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            completed_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_witness_generations_lookup
            ON witness_generations(project, file, status, id);
        CREATE INDEX IF NOT EXISTS idx_witness_generations_skip
            ON witness_generations(project, file, head_oid, extractor_version, status);",
    )?;

    // witness_verdicts (v10 "dreaming" — see `dream` module +
    // `storage::witness_verdicts`'s module doc): append-only EVENTS describing
    // how a `witness_ledger` row's claim relates to git history, minted by the
    // deterministic successor join (`dream::run_dream`), never by hand. Same
    // append-only discipline as `witness_ledger` itself — INSERT + QUERY only,
    // no UPDATE/DELETE. Events are per-witness history: the LATEST event
    // (highest `id`) for a given `witness_id` is that witness's current state;
    // a reinstatement (the A -> B -> A revert case) is a NEW `anchor_reinstated`
    // event layered on top, never an update of the prior negative verdict.
    //
    // `verdict` CHECK constraint keeps the table from ever silently accepting a
    // fourth pseudo-verdict — mirrors `code_node_attribution.channel`'s CHECK
    // (both close the same class of "accidental new state, never validated"
    // bug at the DB layer, not just by convention). `successor_witness_id` is
    // set only for `superseded_by` (the OLDER witness's replacement); NULL for
    // `anchor_obsolete`/`anchor_reinstated`. `receipt_oid` is the commit that
    // MADE the change, found in git history by `dream::receipt` (the first
    // commit that deleted the symbol / changed its normalized content / restored
    // it) -- never the observed HEAD and never the successor witness's `at_oid`.
    // Events minted before the v2 epoch floor used those stand-ins and are
    // retired (see `witness_verdicts::LIVE_EVENT_FLOOR_SQL`). `observed_head_oid` is NOT NULL —
    // every event is anchored to the HEAD commit the dream cycle that minted it
    // saw (per-repo HEAD when a run visits multiple repos), never wall-clock
    // time.
    //
    // Idempotency is APP-SIDE ONLY (`witness_verdicts::insert_verdict_if_changed`):
    // before inserting, the writer reads the LATEST live event for that witness
    // and skips iff the candidate has the same VERDICT KIND (event identity is
    // the verdict kind only: the same conclusion at a newer observed_head_oid,
    // receipt_oid or successor is not a new event). There is deliberately NO UNIQUE identity
    // index on this table: event history legitimately re-visits earlier states
    // (B -> A -> B: superseded, reinstated, superseded again with the exact
    // same fields as the first event) and a UNIQUE index would silently swallow
    // the third event via `INSERT OR IGNORE`, freezing the witness's state at
    // "reinstated" forever. Only the LATEST event per witness matters, so
    // "skip iff the latest has the same verdict kind" is the whole idempotency
    // contract.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS witness_verdicts (
            id INTEGER PRIMARY KEY,
            witness_id INTEGER NOT NULL,
            verdict TEXT NOT NULL CHECK (verdict IN ('anchor_obsolete','anchor_reinstated','superseded_by')),
            successor_witness_id INTEGER,
            receipt_oid TEXT,
            observed_head_oid TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_witness_verdicts_witness ON witness_verdicts(witness_id);
        DROP INDEX IF EXISTS idx_witness_verdicts_identity;

        CREATE TABLE IF NOT EXISTS witness_chunk_bindings (
            witness_id INTEGER NOT NULL REFERENCES witness_ledger(id),
            chunk_id TEXT NOT NULL,
            PRIMARY KEY (witness_id, chunk_id)
        );
        CREATE INDEX IF NOT EXISTS idx_witness_chunk_bindings_chunk
            ON witness_chunk_bindings(chunk_id);",
    )?;

    ensure_witness_verdicts_receipt_committed_at(conn)?;
    ensure_witness_verdicts_v2_floor(conn)?;

    // Recap normalizes SQLite and RFC3339 timestamps through julianday(). This
    // partial covering index matches that expression and the feed's newest-
    // first ordering, so LIMIT can stop after three project matches.
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_witness_verdicts_recap_created
            ON witness_verdicts(
                julianday(created_at) DESC,
                id DESC,
                witness_id,
                verdict,
                receipt_oid,
                created_at
            )
            WHERE verdict IN ('superseded_by', 'anchor_obsolete');",
    )?;

    // Recap's still-open bucket starts at newest unresolved ledger events and
    // joins chunks by primary key. This compact partial index preserves the
    // requested id order without duplicating unbounded claim/evidence text.
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_resolution_open_recent
            ON resolution_ledger(id DESC, chunk_id, status)
            WHERE status IN ('still_open', 'regressed');",
    )?;

    // Recap's settled bucket also credits facts authored inside this session's
    // sidechains (`Storage::recap_ledger_feeds` provenance UNION arm): those
    // chunks carry their own conversation_id but point back to the parent via
    // chunk_provenance.source_conv_id, which had no index before this.
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_chunk_provenance_source_conv
            ON chunk_provenance(source_conv_id);",
    )?;

    // code_nodes conversation-attribution indexes (v10 "dreaming" chunk
    // binding — `storage::chunk_binding::witness_verdict_for_chunks`):
    // `first_conv_id`/`last_conv_id` had no index before this, so every
    // search-time chunk-binding lookup would otherwise full-scan `code_nodes`.
    // Purely additive, cheap to (re)create on existing DBs.
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_code_nodes_first_conv ON code_nodes(first_conv_id);
         CREATE INDEX IF NOT EXISTS idx_code_nodes_last_conv ON code_nodes(last_conv_id);",
    )?;

    // Journal v3 Phase 1.5 — night-pass thread extraction (`dream::threads`).
    // `UNIQUE(episode_hash, thread)` is the convergence key: a re-run over an
    // unchanged episode (same content-hash) either hits the same row again
    // (a real thread, `INSERT OR IGNORE` no-ops) or the sentinel row
    // (`thread = ''`, cached when a run produced zero acceptable threads) —
    // either way, zero further LLM spend. `receipt_tier` is a CHECK, not a
    // free-form column, so a bad write fails loudly instead of silently
    // widening the tier vocabulary the renderer switches on.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS dream_threads (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            episode_hash TEXT NOT NULL,
            session_id TEXT NOT NULL,
            project TEXT NOT NULL,
            thread TEXT NOT NULL,
            evidence_quote TEXT NOT NULL,
            files_json TEXT NOT NULL,
            receipt_tier TEXT NOT NULL CHECK (receipt_tier IN ('verdict','witnessed','unverified')),
            receipts_json TEXT NOT NULL,
            model TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(episode_hash, thread)
        );
        CREATE INDEX IF NOT EXISTS idx_dream_threads_session ON dream_threads(session_id);
        CREATE INDEX IF NOT EXISTS idx_dream_threads_project ON dream_threads(project);",
    )?;

    // Journal v4 Phase 4 — verified structured plans (`journal::composer`).
    // `plan_hash` is the convergence key, computed exactly like
    // `dream::threads::episode_hash`: a re-run over unchanged evidence either
    // finds the stored plan or the sentinel row (`context = ''` with
    // `steps_json = '[]'`, written when verification kept nothing), so a
    // frozen corpus costs zero further spend. `dropped` is the measured
    // count of steps the deterministic verifier removed — it is rendered as
    // a number, never inferred from the difference between two vectors.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS dream_plans (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            plan_hash TEXT NOT NULL UNIQUE,
            item_id TEXT NOT NULL,
            project TEXT NOT NULL,
            session_id TEXT NOT NULL,
            context TEXT NOT NULL,
            steps_json TEXT NOT NULL,
            files_json TEXT NOT NULL,
            acceptance TEXT,
            dropped INTEGER NOT NULL DEFAULT 0,
            model TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_dream_plans_item ON dream_plans(item_id);",
    )?;

    // Journal v4 Phase 4 — per-dream spend attribution (locked decision 13).
    // `narrative_usage` had no key tying a row to the dream that caused it,
    // so a per-dream figure could only ever have been inferred from a
    // timestamp window. `ref_id` makes it evidence instead: the producer
    // writes the convergence hash it was working under, and the composer
    // sums only rows carrying that exact hash. Legacy rows stay NULL and are
    // therefore never attributed to any dream — a dream with no recorded
    // usage renders NOTHING, never a zero that would read as free.
    //
    // Errors PROPAGATE (codex X5 finding 12). The previous form discarded the
    // ALTER/CREATE INDEX result with `let _ =`, so a partial prerelease
    // schema, an interrupted migration or a disk error was accepted as
    // "migrated" while every subsequent `ref_id` insert failed — accounting
    // could disappear without anything refusing to start.
    migrate_narrative_usage_ref_id(conn)?;

    // Journal v4 Phase 4b — dream → outcome attribution (the marker loop).
    //
    // Two row shapes live here, distinguished by `bound_session_id`:
    //
    // * **emission** (`bound_session_id IS NULL`) — a copy block carrying
    //   this dream's marker was rendered, and of which kind. Written by the
    //   surface that rendered it.
    // * **binding** (`bound_session_id IS NOT NULL`) — a transcript
    //   containing the marker was imported. THIS is the evidence: no marker,
    //   no binding, ever. `outcome*` columns are only ever filled on a
    //   binding row, so an unbound dream can render nothing about outcomes.
    //
    // The CHECK keeps an emission row from existing without its kind (the
    // renderer always knows it), while a binding row may legitimately carry
    // `kind IS NULL` — the marker carries a dream id and nothing else, so a
    // binding whose emission row was never recorded genuinely does not know
    // which prompt kind was pasted, and must say so rather than guess.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS dream_attributions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            dream_id TEXT NOT NULL,
            kind TEXT,
            emitted_at TEXT,
            bound_session_id TEXT,
            bound_at TEXT,
            outcome_episode_id TEXT,
            outcome TEXT,
            receipts_json TEXT NOT NULL DEFAULT '[]',
            CHECK (bound_session_id IS NOT NULL OR kind IS NOT NULL)
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_dream_attributions_binding
            ON dream_attributions(dream_id, bound_session_id);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_dream_attributions_emission
            ON dream_attributions(dream_id, kind) WHERE bound_session_id IS NULL;
        CREATE INDEX IF NOT EXISTS idx_dream_attributions_dream
            ON dream_attributions(dream_id);",
    )?;

    // Journal v4 Wave 3 — durable usage reservations.
    //
    // `narrative_usage` is written AFTER a model call returns. A process that
    // dies mid-call, or a producer that discards the insert error, therefore
    // spends real tokens that no row ever records, and the spend figure fails
    // OPEN (reads low, or reads as "unmeasured" when it should read "spent").
    // A reservation is written BEFORE the invocation and finalised after, so
    // the gap between "we are about to spend" and "we know what we spent" is
    // itself a durable row:
    //
    // * `state = 'reserved'` — invocation started, outcome unknown. A row
    //   left in this state is evidence of an unaccounted call, NOT evidence
    //   of zero spend.
    // * `state = 'finalised'` — `usage_id` points at the `narrative_usage`
    //   row that measured it.
    // * `state = 'abandoned'` — the invocation provably never happened
    //   (gate refused, budget exhausted before the call).
    //
    // `attempt_key` is the caller's own idempotency key, so a retried
    // reservation reuses its row instead of double-counting.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS narrative_reservations (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            attempt_key TEXT NOT NULL UNIQUE,
            ref_id TEXT,
            call_site TEXT NOT NULL,
            model TEXT,
            state TEXT NOT NULL DEFAULT 'reserved'
                CHECK (state IN ('reserved','finalised','abandoned')),
            usage_id INTEGER,
            reserved_at TEXT NOT NULL DEFAULT (datetime('now')),
            settled_at TEXT,
            note TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_narrative_reservations_state
            ON narrative_reservations(state);
        CREATE INDEX IF NOT EXISTS idx_narrative_reservations_ref
            ON narrative_reservations(ref_id);",
    )?;

    // Journal v4 Phase 5 — delivery ledger (`storage::dream_delivery`). One
    // row per (conclusion, channel) that was actually shown to the user, so
    // the SessionStart recap clause and the prompt-time match never repeat
    // themselves. The UNIQUE index is what makes "claim it or do not inject"
    // a single atomic step rather than a check-then-write race. A row proves
    // the user was shown something; its absence proves nothing.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS dream_deliveries (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            dream_id TEXT NOT NULL,
            channel TEXT NOT NULL,
            session_id TEXT,
            delivered_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_dream_deliveries_unique
            ON dream_deliveries(dream_id, channel);
        CREATE INDEX IF NOT EXISTS idx_dream_deliveries_at
            ON dream_deliveries(delivered_at);",
    )?;

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS correction_deliveries (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source_session8 TEXT NOT NULL,
            source_turn INTEGER NOT NULL,
            source_date TEXT NOT NULL,
            target_session_id TEXT NOT NULL,
            delivered_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(source_session8, source_turn, source_date, target_session_id)
        );",
    )?;

    // D5 one-shot backfill: rewrite worktree-local paths already stored in
    // code_evolution.file_path / code_nodes.file to canonical main-repo form.
    // Gated by `meta` so it runs exactly once per database, never on every
    // open (canonicalization does a filesystem walk per row).
    if crate::storage::queries::get_meta(conn, "worktree_path_backfill_v1")?.is_none() {
        backfill_worktree_paths(conn)?;
        crate::storage::queries::set_meta(conn, "worktree_path_backfill_v1", "done")?;
    }

    // `csr-engine dreams` (headless CLI) — spend-control cache, NOT an
    // identity hash. `dream_id` is opaque (blake3 of
    // project|category|subject_key|created_at, truncated); the row it names
    // is looked up for reuse by (project, category, revision_hash) — a hit
    // means the underlying receipt set hasn't changed since the prose was
    // authored, so the caller reuses `prose` verbatim and spends nothing.
    // `subject_key` is NULL for the `strategy` category (no deterministic
    // subject exists there — one-shot, no escalation). `status` defaults to
    // 'open' for a future verdict-recording tool to update.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS dreams_v1 (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            dream_id TEXT NOT NULL UNIQUE,
            project TEXT NOT NULL,
            category TEXT NOT NULL CHECK (category IN ('unfinished','strategy','supersession')),
            subject_key TEXT,
            revision_hash TEXT NOT NULL,
            prose TEXT NOT NULL,
            evidence_provenance TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            status TEXT NOT NULL DEFAULT 'open'
        );
        CREATE INDEX IF NOT EXISTS idx_dreams_v1_lookup
            ON dreams_v1(project, category, revision_hash);",
    )?;

    // Dream backfill Stage 6 (`.plans/dream-backfill-design.md` §3 Stage 6):
    // the composer needs a THIRD `dreams_v1` category, 'supersession', added
    // to a table that may already hold real rows from `csr-engine dreams`.
    // SQLite cannot ALTER a CHECK constraint in place, and `CREATE TABLE IF
    // NOT EXISTS` above is a no-op on a pre-existing table — so a DB created
    // before this migration would silently keep the OLD 2-value constraint
    // forever, rejecting every `supersession` insert. Rebuild ONLY when the
    // on-disk constraint doesn't already allow it (shape-probed via
    // `sqlite_master.sql`, same idiom `chunks_fts_schema`/`has_column` use
    // elsewhere in this file) — never unconditionally, since this table can
    // hold real dream rows a user has already read/verdicted and an
    // unconditional drop+recreate would destroy them.
    if !dreams_v1_allows_supersession(conn)? {
        let provenance_copy = if has_column(conn, "dreams_v1", "evidence_provenance")? {
            "evidence_provenance"
        } else {
            "NULL"
        };
        conn.execute_batch(&format!(
            "BEGIN IMMEDIATE;
             ALTER TABLE dreams_v1 RENAME TO dreams_v1_pre_supersession;
             CREATE TABLE dreams_v1 (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                dream_id TEXT NOT NULL UNIQUE,
                project TEXT NOT NULL,
                category TEXT NOT NULL CHECK (category IN ('unfinished','strategy','supersession')),
                subject_key TEXT,
                revision_hash TEXT NOT NULL,
                prose TEXT NOT NULL,
                evidence_provenance TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                status TEXT NOT NULL DEFAULT 'open'
             );
             INSERT INTO dreams_v1 (id, dream_id, project, category, subject_key, revision_hash,
                                    prose, evidence_provenance, created_at, status)
                SELECT id, dream_id, project, category, subject_key, revision_hash,
                       prose, {provenance_copy}, created_at, status
                FROM dreams_v1_pre_supersession;
             DROP TABLE dreams_v1_pre_supersession;
             CREATE INDEX IF NOT EXISTS idx_dreams_v1_lookup
                ON dreams_v1(project, category, revision_hash);
             COMMIT;"
        ))?;
    }

    // Build 1 subagent citations: additive and nullable by design. Existing
    // dreams remain valid historical rows; only newly composed backfill
    // dreams carry the deterministic transcript receipts.
    if !has_column(conn, "dreams_v1", "evidence_provenance")? {
        add_column(
            conn,
            "dreams_v1",
            "evidence_provenance",
            "ALTER TABLE dreams_v1 ADD COLUMN evidence_provenance TEXT",
        )?;
    }

    // Memory registry (harness file-based memory spine — never embedded, never
    // injected). One row per on-disk memory .md file; scanned by a later-stage
    // importer. content_hash + file_mtime drive change detection; last_seen_scan
    // enables project-scoped stale deletion without touching unscanned projects.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS memory_registry (
            file_path        TEXT PRIMARY KEY,
            project           TEXT NOT NULL,
            slug              TEXT NOT NULL,
            description       TEXT,
            mem_type          TEXT,
            origin_session_id TEXT,
            modified_ts       TEXT,
            file_mtime        INTEGER NOT NULL,
            content_hash      TEXT NOT NULL,
            links_json        TEXT NOT NULL DEFAULT '[]',
            last_seen_scan    INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_memory_registry_origin ON memory_registry(origin_session_id);
        CREATE INDEX IF NOT EXISTS idx_memory_registry_project ON memory_registry(project);",
    )?;

    // Trained re-ranker: exact hook impressions, auditable reaction labels,
    // append-only model attempts, and per-cluster gate receipts. The runtime
    // remains deterministic unless the latest model row carries a passing gate.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS rerank_exposure_impressions (
            impression_id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            project TEXT NOT NULL,
            surface TEXT NOT NULL,
            query_hash TEXT,
            query_embedding BLOB,
            intent TEXT NOT NULL,
            shown_at TEXT NOT NULL,
            feature_schema INTEGER NOT NULL,
            item_count INTEGER NOT NULL,
            legacy INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_rerank_impressions_session_time
            ON rerank_exposure_impressions(session_id, shown_at);
        CREATE INDEX IF NOT EXISTS idx_rerank_impressions_time
            ON rerank_exposure_impressions(shown_at);

        CREATE TABLE IF NOT EXISTS rerank_exposure_items (
            impression_id TEXT NOT NULL REFERENCES rerank_exposure_impressions(impression_id)
                ON DELETE CASCADE,
            rank INTEGER NOT NULL,
            memory_id TEXT NOT NULL,
            conversation_id TEXT,
            source_type TEXT NOT NULL,
            baseline_score REAL,
            cosine REAL,
            recency REAL,
            graph_proximity REAL,
            author TEXT,
            is_scaffold INTEGER NOT NULL DEFAULT 0,
            is_mechanic INTEGER NOT NULL DEFAULT 0,
            supersedes INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (impression_id, rank),
            UNIQUE (impression_id, memory_id, source_type)
        );
        CREATE INDEX IF NOT EXISTS idx_rerank_exposure_items_memory
            ON rerank_exposure_items(memory_id);

        CREATE TABLE IF NOT EXISTS rerank_reaction_labels (
            session_id TEXT NOT NULL,
            assistant_turn INTEGER NOT NULL,
            next_user_turn INTEGER NOT NULL,
            assistant_ts TEXT,
            next_user_ts TEXT,
            reaction TEXT NOT NULL CHECK (reaction IN
                ('acceptance','correction','reask','redirect','abstain')),
            proposed_reaction TEXT,
            confidence REAL NOT NULL,
            runner_up_score REAL NOT NULL,
            margin REAL NOT NULL,
            pickup_similarity REAL,
            next_user_text TEXT NOT NULL,
            near_miss INTEGER NOT NULL DEFAULT 0,
            classifier_hash TEXT NOT NULL,
            transcript_mtime INTEGER NOT NULL,
            harvested_at TEXT NOT NULL,
            PRIMARY KEY (session_id, assistant_turn, classifier_hash)
        );
        CREATE INDEX IF NOT EXISTS idx_rerank_reactions_session_time
            ON rerank_reaction_labels(session_id, assistant_ts);
        CREATE INDEX IF NOT EXISTS idx_rerank_reactions_audit
            ON rerank_reaction_labels(classifier_hash, reaction, near_miss);

        CREATE TABLE IF NOT EXISTS rerank_harvest_state (
            session_id TEXT NOT NULL,
            classifier_hash TEXT NOT NULL,
            transcript_mtime INTEGER NOT NULL,
            label_count INTEGER NOT NULL,
            contaminated INTEGER NOT NULL DEFAULT 0,
            harvested_at TEXT NOT NULL,
            PRIMARY KEY (session_id, classifier_hash)
        );

        CREATE TABLE IF NOT EXISTS rerank_models (
            model_id TEXT PRIMARY KEY,
            feature_schema INTEGER NOT NULL,
            classifier_hash TEXT NOT NULL,
            seed INTEGER NOT NULL,
            cutoff_ts TEXT,
            train_start_ts TEXT,
            train_end_ts TEXT,
            eval_start_ts TEXT,
            eval_end_ts TEXT,
            train_impressions INTEGER NOT NULL,
            train_rows INTEGER NOT NULL,
            eval_impressions INTEGER NOT NULL,
            eval_rows INTEGER NOT NULL,
            eval_clusters INTEGER NOT NULL,
            cluster_wins INTEGER NOT NULL,
            cluster_losses INTEGER NOT NULL,
            cluster_ties INTEGER NOT NULL,
            excluded_contaminated INTEGER NOT NULL,
            abstained_reactions INTEGER NOT NULL,
            acceptance_labels INTEGER NOT NULL,
            correction_labels INTEGER NOT NULL,
            reask_labels INTEGER NOT NULL,
            redirect_labels INTEGER NOT NULL,
            near_miss_labels INTEGER NOT NULL,
            baseline_ndcg5 REAL,
            trained_ndcg5 REAL,
            baseline_mrr REAL,
            trained_mrr REAL,
            curated_baseline_score REAL,
            curated_trained_score REAL,
            curated_case_count INTEGER NOT NULL DEFAULT 0,
            curated_veto_epsilon REAL NOT NULL,
            gate_status TEXT NOT NULL CHECK (gate_status IN
                ('passed','failed','insufficient_data','error')),
            gate_reason TEXT NOT NULL,
            weights_json TEXT,
            normalization_json TEXT,
            trained_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_rerank_models_latest
            ON rerank_models(trained_at DESC, model_id DESC);

        CREATE TABLE IF NOT EXISTS rerank_gate_clusters (
            model_id TEXT NOT NULL REFERENCES rerank_models(model_id),
            cluster_id TEXT NOT NULL,
            impression_count INTEGER NOT NULL,
            distinct_session_count INTEGER NOT NULL,
            candidate_count INTEGER NOT NULL,
            baseline_ndcg5 REAL NOT NULL,
            trained_ndcg5 REAL NOT NULL,
            outcome TEXT NOT NULL CHECK (outcome IN ('win','loss','tie')),
            PRIMARY KEY (model_id, cluster_id)
        );",
    )?;

    // Curated-eval veto receipt. These columns were added after the first
    // trained-reranker implementation was reviewable, so prerelease databases
    // may already carry rerank_models without them.
    if !has_column(conn, "rerank_models", "curated_baseline_score")? {
        add_column(
            conn,
            "rerank_models",
            "curated_baseline_score",
            "ALTER TABLE rerank_models ADD COLUMN curated_baseline_score REAL;",
        )?;
    }
    if !has_column(conn, "rerank_models", "curated_trained_score")? {
        add_column(
            conn,
            "rerank_models",
            "curated_trained_score",
            "ALTER TABLE rerank_models ADD COLUMN curated_trained_score REAL;",
        )?;
    }
    if !has_column(conn, "rerank_models", "curated_veto_epsilon")? {
        add_column(
            conn,
            "rerank_models",
            "curated_veto_epsilon",
            "ALTER TABLE rerank_models ADD COLUMN curated_veto_epsilon REAL;",
        )?;
    }
    if !has_column(conn, "rerank_models", "curated_case_count")? {
        add_column(
            conn,
            "rerank_models",
            "curated_case_count",
            "ALTER TABLE rerank_models ADD COLUMN curated_case_count INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    if !has_column(conn, "rerank_gate_clusters", "distinct_session_count")? {
        add_column(
            conn,
            "rerank_gate_clusters",
            "distinct_session_count",
            "ALTER TABLE rerank_gate_clusters
             ADD COLUMN distinct_session_count INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    // Probe first: an UPDATE takes the write lock even when no row matches,
    // and this runs on every storage open.
    let unset_epsilon: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM rerank_models WHERE curated_veto_epsilon IS NULL)",
        [],
        |row| row.get(0),
    )?;
    if unset_epsilon {
        conn.execute(
            "UPDATE rerank_models SET curated_veto_epsilon = ?1
             WHERE curated_veto_epsilon IS NULL",
            [super::trained_rerank::CURATED_VETO_EPSILON],
        )?;
    }

    // Dream backfill (`.plans/dream-backfill-design.md`, Stage 0 + the D3/D4
    // round-2 deltas in its §8, which bind over §3-6 wherever they conflict)
    // — deterministic episode materialization + relation staging for the
    // (future) supersession-detection pipeline. `episode_index` is a
    // REFRESHABLE projection over `reflections` rows carrying schema-v2
    // episode JSON — never the writer of record for episode content
    // (`hooks::stop::store_episode` remains that); it exists so later
    // pipeline stages scan one flat table instead of re-parsing every
    // reflection's JSON on every pass. `storage::dream_backfill::
    // materialize_episode_index` fully replaces every row's base columns on
    // each run (`INSERT OR REPLACE` keyed by `episode_id`), so aliveness
    // (below) is always recomputed alongside a base refresh, never preserved
    // stale against a base row that has moved on.
    //
    // D3 (round-2 finding F3): the aliveness columns replace the design's
    // originally drafted `live_file_ratio REAL`. `present_at_head` /
    // `days_since_last_touch` are NULL — never `false` / `0` — when the
    // episode's project has no git-resolvable repo locally, or when none of
    // its touched files resolve to one: absence of evidence is not
    // deadness, and a NULL here must be excluded and the aliveness term
    // renormalized by the reader (see `storage::dream_backfill::
    // fill_aliveness`'s doc comment), never treated as a zero.
    //
    // `dream_relations` / `backfill_state` / `backfill_discards` are created
    // here so every later build stage (pair generators, adjudicate, verify,
    // compose — design §3 Stages 2-6) has its landing tables from the
    // start; this migration and `storage::dream_backfill` only create and
    // document the three, and write to none of them.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS episode_index (
            episode_id             TEXT PRIMARY KEY,     -- reflections.id
            session_id             TEXT NOT NULL,
            project                TEXT NOT NULL,
            ts                     TEXT NOT NULL,
            outcome                TEXT NOT NULL,
            request                TEXT NOT NULL DEFAULT '',
            completed              TEXT NOT NULL DEFAULT '',
            next_steps             TEXT,
            blockers               TEXT,
            todo_count             INTEGER NOT NULL DEFAULT 0,
            files_json             TEXT NOT NULL DEFAULT '[]',
            anchors_json           TEXT NOT NULL DEFAULT '[]',
            prev_episode_id        TEXT,
            present_at_head        INTEGER,
            days_since_last_touch  INTEGER,
            refreshed_at           TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_episode_index_project_ts ON episode_index(project, ts);
        CREATE INDEX IF NOT EXISTS idx_episode_index_session ON episode_index(session_id);
        CREATE INDEX IF NOT EXISTS idx_episode_index_prev ON episode_index(prev_episode_id);

        -- Stage 6 verified relations. `UNIQUE(project, ep_a, ep_b, relation)`
        -- is the design's own idempotency key (§3 Stage 6); `topic_key` is
        -- D7's stable diversity key (project,symbol) for ledger/relapse,
        -- hash of the shared-anchor-symbol set for era). `generator`/`tier`
        -- vocabularies follow D2 (G-relapse added) and D5 (LLM tier can
        -- never exceed the generator's evidence ceiling) respectively.
        CREATE TABLE IF NOT EXISTS dream_relations (
            id               INTEGER PRIMARY KEY AUTOINCREMENT,
            project          TEXT NOT NULL,
            ep_a             TEXT NOT NULL,   -- episode_index.episode_id, superseded side
            ep_b             TEXT NOT NULL,   -- episode_index.episode_id, superseding side
            relation         TEXT NOT NULL CHECK (relation IN ('replaced_by','extended_by')),
            generator        TEXT NOT NULL CHECK (generator IN ('ledger','era','relapse')),
            topic_key        TEXT NOT NULL,
            tier             TEXT NOT NULL CHECK (tier IN ('verdict','witnessed','unverified')),
            quote_a          TEXT NOT NULL DEFAULT '',
            quote_b          TEXT NOT NULL DEFAULT '',
            load_bearing_oid TEXT,
            aux_oid          TEXT,
            oid_provenance   TEXT NOT NULL DEFAULT 'git_derived'
                CHECK (oid_provenance IN ('git_derived','created_at_fallback')),
            gate_score       REAL,
            now_hook         TEXT,
            status           TEXT NOT NULL DEFAULT 'queued'
                CHECK (status IN ('queued','drained','archived')),
            dream_id         TEXT,
            created_at       TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(project, ep_a, ep_b, relation)
        );
        CREATE INDEX IF NOT EXISTS idx_dream_relations_queue
            ON dream_relations(project, status, gate_score DESC);
        CREATE INDEX IF NOT EXISTS idx_dream_relations_topic
            ON dream_relations(project, topic_key);

        -- Crash-safety checkpoint (design §3 'Crash safety / idempotency').
        -- `project = ''` means an all-projects run; `cursor` is an opaque
        -- resume marker private to whichever stage owns it.
        CREATE TABLE IF NOT EXISTS backfill_state (
            stage      TEXT NOT NULL,
            project    TEXT NOT NULL DEFAULT '',
            cursor     TEXT,
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (stage, project)
        );

        -- Stage 5 verify-failure audit log (design §3 Stage 5 / §6). Every
        -- discarded LLM candidate lands here with its reason and raw output,
        -- so the discard rate is a measurable run-quality metric rather than
        -- a number nothing backs.
        CREATE TABLE IF NOT EXISTS backfill_discards (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            pair_key   TEXT NOT NULL,
            reason     TEXT NOT NULL,
            raw_json   TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_backfill_discards_pair ON backfill_discards(pair_key);
        CREATE INDEX IF NOT EXISTS idx_backfill_discards_reason ON backfill_discards(reason);

        -- Stage 1 unfinished-scan negative-receipt audit trail (design §3
        -- Stage 1 + §8 D10): one row per currently-'never picked up' seed,
        -- `INSERT OR REPLACE` keyed by `seed_episode_id` so a re-scan always
        -- reflects the latest pass — `dream::backfill::unfinished::
        -- scan_unfinished` also deletes any row here for a seed that no
        -- longer qualifies as 'never picked up' on the current corpus (chain-
        -- picked, score-picked, obsolescence-converted, or dropped out of the
        -- seed set entirely), so this table never accumulates stale claims.
        -- `confusion_matrix_json` is the GLOBAL tau-fit's own confusion
        -- matrix at the time of the run, denormalized onto every row so the
        -- receipt is fully self-contained (D10: 'publish confusion matrix in
        -- the negative receipt JSON').
        CREATE TABLE IF NOT EXISTS backfill_unfinished_receipts (
            seed_episode_id       TEXT PRIMARY KEY,  -- episode_index.episode_id
            project               TEXT NOT NULL,
            corpus_scanned        INTEGER NOT NULL,
            max_score             REAL NOT NULL,
            argmax_episode        TEXT,
            confusion_matrix_json TEXT NOT NULL,
            created_at            TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_backfill_unfinished_receipts_project
            ON backfill_unfinished_receipts(project);",
    )?;

    // B2 intent channel v1: immutable, receipted human corrections/redirects
    // and explicit assistant abandonment statements. Idempotency is the
    // transcript identity tuple; later observations append, never rewrite.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS intent_events (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id      TEXT NOT NULL,
            project         TEXT NOT NULL,
            turn            INTEGER NOT NULL,
            kind            TEXT NOT NULL CHECK (kind IN ('correction','redirect','abandoned')),
            quote           TEXT NOT NULL,
            transcript_path TEXT NOT NULL,
            byte_start      INTEGER NOT NULL,
            byte_end        INTEGER NOT NULL,
            prior_claim     TEXT NOT NULL DEFAULT '',
            symbol          TEXT,
            file            TEXT,
            classifier_hash TEXT NOT NULL,
            ts              TEXT NOT NULL,
            created_at      TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(session_id, turn, kind, byte_start)
        );
        CREATE INDEX IF NOT EXISTS idx_intent_events_project_ts
            ON intent_events(project, ts);
        CREATE INDEX IF NOT EXISTS idx_intent_events_project_symbol
            ON intent_events(project, symbol);
        CREATE TRIGGER IF NOT EXISTS intent_events_no_update
            BEFORE UPDATE ON intent_events
            BEGIN SELECT RAISE(ABORT, 'intent_events is append-only'); END;
        CREATE TRIGGER IF NOT EXISTS intent_events_no_delete
            BEFORE DELETE ON intent_events
            BEGIN SELECT RAISE(ABORT, 'intent_events is append-only'); END;",
    )?;
    migrate_intent_events_identity_v2(conn)?;
    migrate_intent_events_detector_v3(conn)?;
    migrate_artifact_provenance(conn)?;
    migrate_strip_memory_tag_from_derived_reflections(conn)?;

    // Reaction-judge shadow mode (src/reaction_shadow.rs): per-transcript scan
    // state and one row per (human turn, model build). Written by the daemon
    // only; nothing in the hook, search or injection path reads these tables.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS reaction_shadow_files (
            path       TEXT PRIMARY KEY,
            mtime_ns   INTEGER NOT NULL,
            size       INTEGER NOT NULL,
            entrypoint TEXT,
            models     TEXT NOT NULL,
            pairs      INTEGER NOT NULL DEFAULT 0,
            complete   INTEGER NOT NULL DEFAULT 0,
            scanned_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS reaction_shadow_scores (
            turn_uuid              TEXT NOT NULL,
            model                  TEXT NOT NULL,
            model_sha256           TEXT NOT NULL,
            session_id             TEXT NOT NULL,
            project_dir            TEXT NOT NULL,
            ts                     TEXT,
            p_correction           REAL NOT NULL,
            p_acceptance           REAL NOT NULL,
            n_tokens               INTEGER NOT NULL,
            previous_human_request TEXT NOT NULL,
            assistant_tail         TEXT NOT NULL,
            human_turn             TEXT NOT NULL,
            scored_at              TEXT NOT NULL,
            PRIMARY KEY (turn_uuid, model_sha256)
        );
        CREATE INDEX IF NOT EXISTS idx_reaction_shadow_session
            ON reaction_shadow_scores(session_id);
        CREATE INDEX IF NOT EXISTS idx_reaction_shadow_ts
            ON reaction_shadow_scores(ts);",
    )?;

    // Transcript mirror (src/mirror.rs): how much of each source transcript the
    // scrubbed, zstd-compressed copy under <data_dir>/mirror/ already covers.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS transcript_mirror (
            rel_path       TEXT PRIMARY KEY,
            source_bytes   INTEGER NOT NULL,
            tail_hash      TEXT NOT NULL,
            zst_bytes      INTEGER NOT NULL,
            frames         INTEGER NOT NULL,
            sealed         INTEGER NOT NULL DEFAULT 0,
            updated_at     TEXT NOT NULL,
            source_gone_at TEXT
        );",
    )?;

    // Conversations whose chunk rows predate the secret scrubber (src/storage/
    // legacy_frontier.rs): those rows stay as stored, and import resumes after
    // the last message they cover.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS legacy_import_frontier (
            conversation_id TEXT PRIMARY KEY,
            after_seq       INTEGER NOT NULL,
            first_chunk     INTEGER NOT NULL,
            recorded_at     TEXT NOT NULL
        );",
    )?;

    // Legacy conversations import tried to place and could not (the same file): a
    // row per conversation, dropped when its frontier is recorded. `status` counts
    // them as waiting.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS legacy_import_unplaced (
            conversation_id TEXT PRIMARY KEY,
            first_tried_at  TEXT NOT NULL,
            last_tried_at   TEXT NOT NULL,
            tries           INTEGER NOT NULL DEFAULT 1
        );",
    )?;

    // Prompts the person typed while Claude was busy (src/storage/queued_prompts.rs):
    // one row per chunk written for such a prompt, keyed by the transcript line it came
    // from, so a prompt is indexed once however many passes see it. The chunk rows
    // are ordinary `chunks` rows; this table is how they are told apart from the
    // conversation's own.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS queued_prompt_chunks (
            chunk_id        TEXT PRIMARY KEY,
            conversation_id TEXT NOT NULL,
            line_offset     INTEGER NOT NULL,
            part            INTEGER NOT NULL,
            indexed_at      TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_queued_prompt_chunks_prompt
            ON queued_prompt_chunks(conversation_id, line_offset);",
    )?;

    // Conversations whose transcript the queued-prompt pass has read: the one-time
    // backfill's record that it ran, and what it found.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS queued_prompt_scan (
            conversation_id TEXT PRIMARY KEY,
            scanned_at      TEXT NOT NULL,
            found           INTEGER NOT NULL,
            added           INTEGER NOT NULL
        );",
    )?;

    // Code map (src/codemap): a per-worktree map of every definition in the user's
    // repos. Additive only (IF NOT EXISTS, nothing dropped or altered), so
    // re-running this on an up-to-date database writes nothing. The tables that the
    // freshness job (`codemap_queue`) and the consumers (`codemap_fts`) need are
    // created here with the rest, so they add no migration of their own.
    conn.execute_batch(CREATE_CODEMAP)?;

    create_graph_gen_triggers(conn)?;

    // Exact-match index of identifiers (paths, symbols, flags, SHAs). Additive.
    super::identifier_postings::migrate_identifier_postings(conn)?;

    finish_chunks_fts_compaction(conn)?;

    Ok(())
}

/// Create `code_graph_project_settled` and bump the counter of every project
/// with graph content, so each is resolved once (see the migration above). The
/// table's existence is what says the bump was done, so the two are ONE
/// transaction: a process that dies between them would otherwise reopen to a
/// table with no bump, and a project with only `repo_defs` would never be
/// resolved. Up to date, it is a read and takes no write lock.
fn create_settled_table(conn: &Connection) -> Result<()> {
    let exists = |conn: &Connection| -> Result<bool> {
        Ok(conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'code_graph_project_settled')",
            [],
            |r| r.get(0),
        )?)
    };
    if exists(conn)? {
        return Ok(());
    }
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    // Another process may have done it while this one waited for the lock.
    if exists(&tx)? {
        return Ok(());
    }
    tx.execute_batch(
        "CREATE TABLE code_graph_project_settled (
             project    TEXT PRIMARY KEY,
             own        INTEGER NOT NULL DEFAULT 0,
             star       INTEGER NOT NULL DEFAULT 0,
             settled_at INTEGER NOT NULL DEFAULT 0
         );
         INSERT INTO code_graph_project_gen (project, gen)
             SELECT project, 1 FROM (
                 SELECT DISTINCT project FROM code_nodes
                 UNION SELECT DISTINCT project FROM repo_defs
                 UNION SELECT DISTINCT project FROM code_graph_file_state
             ) WHERE project IS NOT NULL AND project != ''
             ON CONFLICT(project) DO UPDATE SET gen = gen + 1;",
    )?;
    tx.commit()?;
    Ok(())
}

/// Project that a table's gen bump is for, in the trigger SQL below.
const GEN_BUMP_PROJECT: &str = "INSERT INTO code_graph_project_gen (project, gen) VALUES ({p}, 1)
     ON CONFLICT(project) DO UPDATE SET gen = gen + 1;";

/// `meta` key holding the revision of the trigger bodies installed by
/// [`create_graph_gen_triggers`]. `CREATE TRIGGER IF NOT EXISTS` never replaces
/// a body, so a changed body needs a new revision to be swapped in once.
const GRAPH_GEN_TRIGGERS_REV_KEY: &str = "graph_gen_triggers_rev";

/// Revision 3: no write bumps every project any more, and the co-edit counters
/// live in their own table (see below).
const GRAPH_GEN_TRIGGERS_REV: &str = "3";

/// Triggers whose body changed since revision 1.
const GRAPH_GEN_TRIGGERS_CHANGED: [&str; 6] = [
    "code_edges_gen_ai",
    "code_edges_gen_au",
    "code_edges_gen_ad",
    "code_evolution_gen_ai",
    "code_evolution_gen_au",
    "code_evolution_gen_ad",
];

/// Edge write: bump the source node's project. An edge with no source node is
/// read by no resolve (every edge read in `resolver::resolve_edges` joins
/// `code_nodes` on `src_id`) but it still counts toward its destination's
/// in-degree in `code_rank::rank_rows`, so the bump goes to the destination's
/// project instead of to every project.
fn edge_src_bump(row: &str) -> String {
    format!(
        "INSERT INTO code_graph_project_gen (project, gen)
           SELECT n.project, 1 FROM code_nodes n WHERE n.id = {row}.src_id
           ON CONFLICT(project) DO UPDATE SET gen = gen + 1;"
    )
}

/// The destination-project bump of an edge whose source node is gone.
fn edge_orphan_dst_bump(row: &str) -> String {
    format!(
        "INSERT INTO code_graph_project_gen (project, gen)
           SELECT d.project, 1 FROM code_nodes d
           WHERE d.id = {row}.dst_id
             AND NOT EXISTS (SELECT 1 FROM code_nodes s WHERE s.id = {row}.src_id)
           ON CONFLICT(project) DO UPDATE SET gen = gen + 1;"
    )
}

/// SQL for the last path component of `p`, a path expression. A path with no
/// `/` is its own last component. The same expression on `code_nodes.file` is
/// the key of `idx_code_nodes_basename`, so the two sides must stay spelled alike.
pub(crate) fn basename_sql(p: &str) -> String {
    format!("substr({p}, length(rtrim({p}, replace({p}, '/', ''))) + 1)")
}

/// Co-edit evidence written by a `code_evolution` row (`file`, `project`
/// expressions): bump the co-edit counter of every project that may read it.
///
/// What is read: the co-edit tier (`resolver::coedit_weight`) scores a pair of
/// files `(f, d)` of ONE project by the sessions that touched both, comparing the
/// row's `file_path` with `canon_path(node.file)`. So a row for path `x` changes
/// the answer of project `P` only if `canon(f)` or `canon(d)` is `x` for some node
/// file of `P`. The trigger cannot call `canon`, and it depends on the file system
/// (a linked worktree's `.git` file), so it covers every spelling `canon` can
/// give instead: `canon` keeps the repo-relative tail of a path when it maps a
/// linked worktree onto its main checkout, and resolves directory symlinks
/// (`/tmp` to `/private/tmp`), so the last path component of a node's file is the
/// last path component of its canonical form. A project is bumped when
///
///   1. one of its nodes' files ends in the row's last path component, or
///   2. it is the project the row is labelled with (`project_name`), which is the
///      project the hook resolved the edited path to; this also covers a legacy
///      node stored under a symlink whose target has another name.
///
/// That is a superset of what the tier reads. It over-bumps (a row for `mod.rs`
/// reaches every project with a `mod.rs`), which costs at most one pass per
/// project per `STAR_ONLY_COOLDOWN_SECS`, because these counters only ask for a
/// pass after the cooldown. Both lookups are indexed (`idx_code_nodes_basename`).
///
/// A node that appears in the file later is covered by the node's own trigger.
///
/// The counters are rows of `code_graph_project_coedit`, not of
/// `code_graph_project_gen`: a binary from before this table lists the project
/// counter rows as projects, and would treat these as projects.
fn coedit_bump(file: &str, project: &str) -> String {
    let base = basename_sql(file);
    let node_base = basename_sql("n.file");
    format!(
        "INSERT INTO code_graph_project_coedit (project, gen)
           SELECT n.project, 1 FROM code_nodes n
           WHERE {node_base} = {base}
             AND n.project IS NOT NULL AND n.project != ''
           GROUP BY n.project
           ON CONFLICT(project) DO UPDATE SET gen = gen + 1;
         INSERT INTO code_graph_project_coedit (project, gen)
           SELECT {project}, 1 WHERE {project} IS NOT NULL AND {project} != ''
           ON CONFLICT(project) DO UPDATE SET gen = gen + 1;"
    )
}

/// `code_graph_project_gen` triggers. The drain plans a project's resolve
/// without the write lock and applies it only if the project's counter has not
/// moved, so the counter has to move for EVERY write to a table the resolve
/// reads, from whatever code path writes it (hook, drain, backfill, co-edit
/// replay, repo scan, a migration, an operator's SQL). A helper that bumps after
/// its statement leaves a window between the two and is skipped by any writer
/// that does not call it; a trigger bumps inside the statement's own
/// transaction and cannot be bypassed.
///
/// Nothing bumps every project. Revision 1 did, for a `code_evolution` row and
/// for an edge with no source node, and a live database then re-resolved all of
/// its projects after each one: a pass over them took longer than the interval
/// between rows, so the drain never went idle. Each write now bumps the
/// projects it can change the answer of (see [`edge_src_bump`] and
/// [`coedit_bump`] for the argument).
///
/// Coexistence with a binary from before revision 3. Such a binary reads only a
/// project's own counter and `'*'` when it checks a plan, so it cannot see a
/// co-edit bump. Who can drain: the daemon, every MCP server (as a follower,
/// when no capable drainer has a fresh heartbeat) and `csr-engine codegraph
/// drain`; the post-tool-use hook only queues. An MCP server of the old build
/// stays up for hours after an install, so the swap pins the heartbeat the old
/// build reads (`graph_drain::LEGACY_HEARTBEAT_PIN`, in the same transaction)
/// and the new build re-pins it on every beat: an old follower always finds a
/// capable drainer and stands down. The new build reads its own key. What is
/// left is an old daemon (the install stops it first) or a hand-run old CLI; a
/// plan of theirs that races a co-edit row can bind from stale weights, and
/// since they settle a project with an every-project value that lacks the
/// project's co-edit counter, the new build re-resolves it after the cooldown.
///
/// The resolver's own edge updates fire the edge trigger too: the plan they
/// belong to has been applied, and the rank plan reads its counter after the
/// apply commits.
///
/// Up to date, this is a read and takes no write lock. A database on an older
/// revision gets the revision-2 bodies in one transaction, and the edges whose
/// source node is gone are deleted in it (a dangling edge has nothing to
/// resolve; `replace_file_edges` would have swept it at the next extraction of
/// its file).
fn create_graph_gen_triggers(conn: &Connection) -> Result<()> {
    let mut sql = String::new();
    let mut trigger = |name: &str, event: &str, table: &str, body: String| {
        sql.push_str(&format!(
            "CREATE TRIGGER IF NOT EXISTS {name} AFTER {event} ON {table} BEGIN {body} END;\n"
        ));
    };
    // Tables that carry their own project column.
    for (table, col) in [
        ("code_nodes", "project"),
        ("repo_defs", "project"),
        ("local_bindings", "project"),
        ("edge_scope_chains", "project"),
    ] {
        trigger(
            &format!("{table}_gen_ai"),
            "INSERT",
            table,
            GEN_BUMP_PROJECT.replace("{p}", &format!("NEW.{col}")),
        );
        trigger(
            &format!("{table}_gen_au"),
            "UPDATE",
            table,
            format!(
                "{} {}",
                GEN_BUMP_PROJECT.replace("{p}", &format!("OLD.{col}")),
                GEN_BUMP_PROJECT.replace("{p}", &format!("NEW.{col}")),
            ),
        );
        trigger(
            &format!("{table}_gen_ad"),
            "DELETE",
            table,
            GEN_BUMP_PROJECT.replace("{p}", &format!("OLD.{col}")),
        );
    }
    // Edges: the source node's project; with no source node, the destination's.
    trigger(
        "code_edges_gen_ai",
        "INSERT",
        "code_edges",
        format!("{} {}", edge_src_bump("NEW"), edge_orphan_dst_bump("NEW")),
    );
    trigger(
        "code_edges_gen_au",
        "UPDATE",
        "code_edges",
        format!(
            "{} {} {}",
            edge_src_bump("NEW"),
            edge_orphan_dst_bump("NEW"),
            edge_orphan_dst_bump("OLD")
        ),
    );
    trigger(
        "code_edges_gen_ad",
        "DELETE",
        "code_edges",
        format!("{} {}", edge_src_bump("OLD"), edge_orphan_dst_bump("OLD")),
    );
    // Co-edit evidence: the projects with a node in the row's file.
    trigger(
        "code_evolution_gen_ai",
        "INSERT",
        "code_evolution",
        coedit_bump("NEW.file_path", "NEW.project_name"),
    );
    trigger(
        "code_evolution_gen_au",
        "UPDATE",
        "code_evolution",
        format!(
            "{} {}",
            coedit_bump("OLD.file_path", "OLD.project_name"),
            coedit_bump("NEW.file_path", "NEW.project_name")
        ),
    );
    trigger(
        "code_evolution_gen_ad",
        "DELETE",
        "code_evolution",
        coedit_bump("OLD.file_path", "OLD.project_name"),
    );

    // The co-edit counters (see `coedit_bump`) and the index its lookup uses.
    // Both are additive and `IF NOT EXISTS`, so up to date they write nothing.
    let schema = format!(
        "CREATE TABLE IF NOT EXISTS code_graph_project_coedit (
             project TEXT PRIMARY KEY,
             gen     INTEGER NOT NULL DEFAULT 0
         );
         CREATE INDEX IF NOT EXISTS idx_code_nodes_basename ON code_nodes ({});",
        basename_sql("file")
    );
    let rev = |c: &Connection| -> Result<Option<String>> {
        Ok(c.query_row(
            "SELECT value FROM meta WHERE key = ?1",
            [GRAPH_GEN_TRIGGERS_REV_KEY],
            |r| r.get(0),
        )
        .optional()?)
    };
    if rev(conn)?.as_deref() == Some(GRAPH_GEN_TRIGGERS_REV) {
        // Up to date; recreates a trigger someone dropped, otherwise a no-op.
        conn.execute_batch(&schema)?;
        conn.execute_batch(&sql)?;
        return Ok(());
    }
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    // Another process may have done it while this one waited for the lock.
    if rev(&tx)?.as_deref() == Some(GRAPH_GEN_TRIGGERS_REV) {
        return Ok(());
    }
    for name in GRAPH_GEN_TRIGGERS_CHANGED {
        tx.execute_batch(&format!("DROP TRIGGER IF EXISTS {name};"))?;
    }
    tx.execute_batch(&schema)?;
    tx.execute_batch(&sql)?;
    // A build that never shipped kept these in the project counter table.
    tx.execute(
        "DELETE FROM code_graph_project_gen WHERE substr(project, 1, 8) = '*coedit:'",
        [],
    )?;
    // Old drainers stand down (see above); same transaction as the new bodies.
    tx.execute(
        "INSERT INTO meta(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [
            crate::extraction::graph_drain::LEGACY_HEARTBEAT_KEY,
            crate::extraction::graph_drain::LEGACY_HEARTBEAT_PIN,
        ],
    )?;
    // After the new triggers exist, so each delete bumps the edge's destination
    // project.
    tx.execute(
        "DELETE FROM code_edges
         WHERE NOT EXISTS (SELECT 1 FROM code_nodes n WHERE n.id = code_edges.src_id)",
        [],
    )?;
    tx.execute(
        "INSERT INTO meta(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [GRAPH_GEN_TRIGGERS_REV_KEY, GRAPH_GEN_TRIGGERS_REV],
    )?;
    tx.commit()?;
    Ok(())
}

/// Code map schema. A repo is a git common dir; a view is one worktree of it;
/// a blob is file content keyed by git blob oid and parsed once per extractor
/// version however many views hold it. `codemap_fts` has one row per distinct
/// (path, blob, def) that at least one view currently holds; its rowid is a
/// hash of those parts, so rows are added and removed without scanning the table.
const CREATE_CODEMAP: &str = "
    CREATE TABLE IF NOT EXISTS codemap_repos (
        common_dir   TEXT PRIMARY KEY,
        main_path    TEXT,
        source       TEXT NOT NULL,
        enabled      INTEGER NOT NULL DEFAULT 1,
        added_at     INTEGER NOT NULL,
        last_poll_at INTEGER,
        status       TEXT NOT NULL DEFAULT 'ok'
    );
    CREATE TABLE IF NOT EXISTS codemap_views (
        id             INTEGER PRIMARY KEY,
        common_dir     TEXT NOT NULL,
        worktree_path  TEXT NOT NULL UNIQUE,
        branch         TEXT,
        head_oid       TEXT,
        index_mtime_ns INTEGER,
        index_size     INTEGER,
        scanned_at     INTEGER,
        status         TEXT NOT NULL,
        files          INTEGER NOT NULL DEFAULT 0,
        truncated      INTEGER NOT NULL DEFAULT 0,
        scan_ms        INTEGER
    );
    CREATE INDEX IF NOT EXISTS idx_codemap_views_common_dir ON codemap_views(common_dir);
    CREATE TABLE IF NOT EXISTS codemap_view_files (
        view_id  INTEGER NOT NULL,
        path     TEXT NOT NULL,
        blob_oid TEXT NOT NULL,
        dirty    INTEGER NOT NULL DEFAULT 0,
        size     INTEGER,
        mtime_ns INTEGER,
        PRIMARY KEY (view_id, path)
    ) WITHOUT ROWID;
    CREATE INDEX IF NOT EXISTS idx_codemap_view_files_blob ON codemap_view_files(blob_oid);
    CREATE INDEX IF NOT EXISTS idx_codemap_view_files_path ON codemap_view_files(path);
    CREATE TABLE IF NOT EXISTS codemap_blobs (
        blob_oid          TEXT NOT NULL,
        extractor_version TEXT NOT NULL,
        lang              TEXT,
        status            TEXT NOT NULL,
        defs              INTEGER NOT NULL DEFAULT 0,
        parsed_at         INTEGER NOT NULL,
        PRIMARY KEY (blob_oid, extractor_version)
    ) WITHOUT ROWID;
    CREATE TABLE IF NOT EXISTS codemap_defs (
        blob_oid          TEXT NOT NULL,
        extractor_version TEXT NOT NULL,
        name              TEXT NOT NULL,
        fqname            TEXT,
        kind              TEXT NOT NULL,
        span_start        INTEGER NOT NULL,
        span_end          INTEGER NOT NULL,
        signature         TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_codemap_defs_blob ON codemap_defs(blob_oid, extractor_version);
    CREATE INDEX IF NOT EXISTS idx_codemap_defs_name ON codemap_defs(name);
    CREATE TABLE IF NOT EXISTS codemap_queue (
        path        TEXT PRIMARY KEY,
        enqueued_at INTEGER NOT NULL
    );
    -- Session signals: the SessionStart and PostToolUse hooks upsert the cwd of a
    -- live session (one statement, no git, no stat). The daemon maps the repos
    -- that have a recent signal; it fills common_dir and status when it resolves
    -- the cwd, so the status report needs no filesystem call.
    CREATE TABLE IF NOT EXISTS codemap_signals (
        cwd        TEXT PRIMARY KEY,
        last_at    INTEGER NOT NULL,
        source     TEXT NOT NULL,
        common_dir TEXT,
        status     TEXT
    );
    CREATE VIRTUAL TABLE IF NOT EXISTS codemap_fts USING fts5(
        name,
        signature,
        path,
        blob_oid UNINDEXED,
        tokenize='porter unicode61'
    );
";

/// `intent_events_v2`: make the transcript part of an event's identity.  A
/// parent session may have many child transcripts whose turns and byte
/// offsets overlap, so the v1 key lost all but the first child event.
fn migrate_intent_events_identity_v2(conn: &Connection) -> Result<()> {
    let sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'intent_events'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let Some(sql) = sql else {
        return Ok(());
    };
    let compact: String = sql.chars().filter(|ch| !ch.is_whitespace()).collect();
    if compact.contains("UNIQUE(session_id,transcript_path,turn,kind,byte_start)") {
        return Ok(());
    }

    conn.execute_batch(
        "DROP TRIGGER IF EXISTS intent_events_no_update;
         DROP TRIGGER IF EXISTS intent_events_no_delete;
         DROP INDEX IF EXISTS idx_intent_events_project_ts;
         DROP INDEX IF EXISTS idx_intent_events_project_symbol;
         ALTER TABLE intent_events RENAME TO intent_events_v1_old;
         CREATE TABLE intent_events (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id      TEXT NOT NULL,
            project         TEXT NOT NULL,
            turn            INTEGER NOT NULL,
            kind            TEXT NOT NULL CHECK (kind IN ('correction','redirect','abandoned')),
            quote           TEXT NOT NULL,
            transcript_path TEXT NOT NULL,
            byte_start      INTEGER NOT NULL,
            byte_end        INTEGER NOT NULL,
            prior_claim     TEXT NOT NULL DEFAULT '',
            symbol          TEXT,
            file            TEXT,
            classifier_hash TEXT NOT NULL,
            ts              TEXT NOT NULL,
            created_at      TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(session_id, transcript_path, turn, kind, byte_start)
         );
         INSERT INTO intent_events
            (id, session_id, project, turn, kind, quote, transcript_path,
             byte_start, byte_end, prior_claim, symbol, file, classifier_hash,
             ts, created_at)
         SELECT id, session_id, project, turn, kind, quote, transcript_path,
                byte_start, byte_end, prior_claim, symbol, file, classifier_hash,
                ts, created_at
           FROM intent_events_v1_old;
         DROP TABLE intent_events_v1_old;
         CREATE INDEX idx_intent_events_project_ts ON intent_events(project, ts);
         CREATE INDEX idx_intent_events_project_symbol ON intent_events(project, symbol);
         CREATE TRIGGER intent_events_no_update
            BEFORE UPDATE ON intent_events
            BEGIN SELECT RAISE(ABORT, 'intent_events is append-only'); END;
         CREATE TRIGGER intent_events_no_delete
            BEFORE DELETE ON intent_events
            BEGIN SELECT RAISE(ABORT, 'intent_events is append-only'); END;",
    )?;
    Ok(())
}

/// `intent_events_v3`: detector provenance and abandonment-marker receipts.
/// These are deliberately additive ALTERs so an interrupted upgrade is
/// repaired one missing column at a time without rebuilding immutable rows.
fn migrate_intent_events_detector_v3(conn: &Connection) -> Result<()> {
    if !has_column(conn, "intent_events", "detector")? {
        add_column(
            conn,
            "intent_events",
            "detector",
            "ALTER TABLE intent_events ADD COLUMN detector TEXT",
        )?;
    }
    if !has_column(conn, "intent_events", "classifier_score")? {
        add_column(
            conn,
            "intent_events",
            "classifier_score",
            "ALTER TABLE intent_events ADD COLUMN classifier_score TEXT",
        )?;
    }
    if !has_column(conn, "intent_events", "marker")? {
        add_column(
            conn,
            "intent_events",
            "marker",
            "ALTER TABLE intent_events ADD COLUMN marker TEXT",
        )?;
    }
    Ok(())
}

/// Does the on-disk `dreams_v1` CHECK constraint already allow the
/// `supersession` category? Answered from `sqlite_master.sql` itself (the
/// literal `CREATE TABLE` text SQLite stores), not from a probe insert —
/// same shape-probe idiom `chunks_fts_schema` uses for `chunks_fts`. A
/// missing table (fresh DB, not yet created) reads as "already allows it":
/// the `CREATE TABLE IF NOT EXISTS` immediately above this call already
/// wrote the 3-value constraint on a fresh DB, so there is nothing to
/// rebuild.
fn dreams_v1_allows_supersession(conn: &Connection) -> Result<bool> {
    let sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'dreams_v1'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(sql.is_none_or(|s| s.contains("supersession")))
}

/// Does `table` have `column`? Answered from `pragma_table_info`, i.e. from
/// the schema itself — not from whether a probe `SELECT` happened to parse.
fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
        rusqlite::params![table, column],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Another opener may add the column after our absence check. Accept only
/// a schema that now contains it; unrelated ALTER failures still propagate.
fn add_column(conn: &Connection, table: &str, column: &str, sql: &str) -> Result<()> {
    if let Err(error) = conn.execute_batch(sql) {
        if !has_column(conn, table, column)? {
            return Err(error.into());
        }
    }
    Ok(())
}

/// Expand the schema with message-bound provenance coordinates. Existing
/// chunks retain an Unknown floor until a structural import or backfill can
/// attach evidence; the migration never infers trust from legacy authorship.
fn migrate_provenance_substrate(conn: &Connection) -> Result<()> {
    if !has_column(conn, "chunks", "min_trust")? {
        add_column(
            conn,
            "chunks",
            "min_trust",
            "ALTER TABLE chunks ADD COLUMN min_trust INTEGER NOT NULL DEFAULT 0
                 CHECK (min_trust BETWEEN 0 AND 5)",
        )?;
    }
    if !has_column(conn, "chunks", "tool_result_share")? {
        add_column(
            conn,
            "chunks",
            "tool_result_share",
            "ALTER TABLE chunks ADD COLUMN tool_result_share REAL
                 CHECK (tool_result_share IS NULL OR
                        (tool_result_share >= 0.0 AND tool_result_share <= 1.0))",
        )?;
    }
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_chunks_min_trust ON chunks(min_trust);

         CREATE TABLE IF NOT EXISTS provenance_events (
            event_id        TEXT PRIMARY KEY,
            conversation_id TEXT NOT NULL,
            message_key     TEXT NOT NULL,
            seq             INTEGER NOT NULL,
            channel         TEXT NOT NULL,
            trust_tier      INTEGER NOT NULL DEFAULT 0
                CHECK (trust_tier BETWEEN 0 AND 5),
            parent_event_id TEXT REFERENCES provenance_events(event_id),
            receipt_kind    TEXT NOT NULL,
            receipt_ref     TEXT,
            observed_at     TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_provenance_events_conversation_seq
            ON provenance_events(conversation_id, seq);

         CREATE TABLE IF NOT EXISTS chunk_spans (
            chunk_id    TEXT NOT NULL REFERENCES chunks(id) ON DELETE CASCADE,
            event_id    TEXT NOT NULL REFERENCES provenance_events(event_id),
            start_char  INTEGER NOT NULL CHECK (start_char >= 0),
            end_char    INTEGER NOT NULL CHECK (end_char >= start_char),
            content_hash TEXT NOT NULL,
            PRIMARY KEY (chunk_id, event_id, start_char, end_char)
         );
         CREATE INDEX IF NOT EXISTS idx_chunk_spans_chunk ON chunk_spans(chunk_id);",
    )?;
    Ok(())
}

/// Additive cached floors: legacy content and agent-writable tags supply no
/// evidence. Keep this after all artifact tables and intent identity migrations.
fn migrate_artifact_provenance(conn: &Connection) -> Result<()> {
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
        if !has_column(conn, table, "min_trust")? {
            add_column(conn, table, "min_trust", &format!("ALTER TABLE {table} ADD COLUMN min_trust INTEGER NOT NULL DEFAULT 0 CHECK(min_trust BETWEEN 0 AND 5)"))?;
        }
    }
    if !has_column(conn, "resolution_ledger", "event_id")? {
        add_column(conn, "resolution_ledger", "event_id", "ALTER TABLE resolution_ledger ADD COLUMN event_id TEXT REFERENCES provenance_events(event_id)")?;
    }
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_provenance_events_parent ON provenance_events(parent_event_id);
         CREATE TABLE IF NOT EXISTS artifact_derivations (
            artifact_kind TEXT NOT NULL,
            artifact_id TEXT NOT NULL,
            artifact_start_char INTEGER NOT NULL CHECK(artifact_start_char >= 0),
            artifact_end_char INTEGER NOT NULL CHECK(artifact_end_char >= artifact_start_char),
            support_event_id TEXT REFERENCES provenance_events(event_id),
            -- Keep the receipt when an importer replaces/deletes a chunk.
            -- Cold-path validation treats a missing/version-mismatched parent
            -- as Unknown; a restrictive FK would block ordinary reimports.
            support_chunk_id TEXT,
            support_start_char INTEGER NOT NULL CHECK(support_start_char >= 0),
            support_end_char INTEGER NOT NULL CHECK(support_end_char >= support_start_char),
            CHECK(support_event_id IS NOT NULL OR support_chunk_id IS NOT NULL)
         );
         CREATE INDEX IF NOT EXISTS idx_artifact_derivations_artifact
            ON artifact_derivations(artifact_kind,artifact_id);
         CREATE INDEX IF NOT EXISTS idx_artifact_derivations_support_event
            ON artifact_derivations(support_event_id);
         CREATE INDEX IF NOT EXISTS idx_provenance_events_snapshot
            ON provenance_events(json_extract(receipt_ref,'$.kind'), json_extract(receipt_ref,'$.id'))
            WHERE receipt_kind='artifact_input';
         CREATE UNIQUE INDEX IF NOT EXISTS idx_artifact_derivations_identity
            ON artifact_derivations(artifact_kind,artifact_id,artifact_start_char,artifact_end_char,
                COALESCE(support_event_id,''),COALESCE(support_chunk_id,''),support_start_char,support_end_char);
         CREATE TRIGGER IF NOT EXISTS confirmation_event_no_update
            BEFORE UPDATE ON provenance_events WHEN OLD.channel='user_confirmation'
            BEGIN SELECT RAISE(ABORT,'confirmation events are immutable'); END;
         CREATE TRIGGER IF NOT EXISTS confirmation_event_no_delete
            BEFORE DELETE ON provenance_events WHEN OLD.channel='user_confirmation'
            BEGIN SELECT RAISE(ABORT,'confirmation events are immutable'); END;"
    )?;
    migrate_intent_events_no_update_scoped(conn)
}

/// Only the `min_trust` cache may change. The complete preexisting intent
/// payload and its transcript identity remain append-only, including nullable
/// fields.
const INTENT_EVENTS_NO_UPDATE_SCOPED: &str =
    "CREATE TRIGGER intent_events_no_update BEFORE UPDATE ON intent_events
        WHEN NEW.id IS NOT OLD.id OR NEW.session_id IS NOT OLD.session_id
          OR NEW.project IS NOT OLD.project OR NEW.turn IS NOT OLD.turn
          OR NEW.kind IS NOT OLD.kind OR NEW.quote IS NOT OLD.quote
          OR NEW.transcript_path IS NOT OLD.transcript_path
          OR NEW.byte_start IS NOT OLD.byte_start OR NEW.byte_end IS NOT OLD.byte_end
          OR NEW.prior_claim IS NOT OLD.prior_claim OR NEW.symbol IS NOT OLD.symbol
          OR NEW.file IS NOT OLD.file OR NEW.classifier_hash IS NOT OLD.classifier_hash
          OR NEW.ts IS NOT OLD.ts OR NEW.created_at IS NOT OLD.created_at
          OR NEW.detector IS NOT OLD.detector OR NEW.classifier_score IS NOT OLD.classifier_score
          OR NEW.marker IS NOT OLD.marker
        BEGIN SELECT RAISE(ABORT,'intent_events is append-only'); END";

fn intent_events_no_update_is_scoped(conn: &Connection) -> Result<bool> {
    let sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = 'intent_events_no_update'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let compact = |sql: &str| -> String { sql.split_whitespace().collect() };
    Ok(sql.is_some_and(|sql| compact(&sql) == compact(INTENT_EVENTS_NO_UPDATE_SCOPED)))
}

/// Replace the blanket `intent_events_no_update` with the column-scoped one,
/// once. Every hook process runs these migrations on open, several at a time
/// at session start, so this must be a pure read on a database that already
/// has the scoped trigger: an unconditional DROP+CREATE here raced between
/// concurrent opens ("trigger intent_events_no_update already exists") and
/// made every open wait for the write lock behind the daemon ("database is
/// locked"). The rewrite itself runs under `BEGIN IMMEDIATE` and re-checks the
/// shape once it holds the lock, so concurrent first upgrades serialize and
/// only the first one writes.
fn migrate_intent_events_no_update_scoped(conn: &Connection) -> Result<()> {
    if intent_events_no_update_is_scoped(conn)? {
        return Ok(());
    }
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let rewrite = (|| -> Result<()> {
        if intent_events_no_update_is_scoped(conn)? {
            return Ok(());
        }
        conn.execute_batch(&format!(
            "DROP TRIGGER IF EXISTS intent_events_no_update;
             {INTENT_EVENTS_NO_UPDATE_SCOPED};"
        ))?;
        Ok(())
    })();
    match rewrite {
        Ok(()) => conn.execute_batch("COMMIT")?,
        Err(err) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(err);
        }
    }
    Ok(())
}

/// Meta key gating [`migrate_strip_memory_tag_from_derived_reflections`] so it
/// scans `reflections` once rather than on every startup.
const STRIP_MEMORY_TAG_FROM_DERIVED_KEY: &str = "strip_csr_memory_tag_from_derived_reflections_v1";

/// One-shot repair for the bug where `store_reflection`'s explicit-memory
/// stamp (`csr:memory`) leaked onto pipeline-derived reflections (stories,
/// episodes, briefings, cache rows, `narrative_*` extractions) because they
/// shared the same insert path. A derived tag on a row means it was never a
/// deliberate memory write; strip the stamp so `is_explicit_memory` /
/// `reflection_recall_standing` stop granting it unearned user standing (the
/// live 2026-09-22 case: reflection `3e4c5285`, tagged `session_story` +
/// `csr:memory`, scored `adj 0.994 = cos 0.494 + user 0.50`).
///
/// Gated by [`STRIP_MEMORY_TAG_FROM_DERIVED_KEY`] so the scan runs once, not
/// on every `migrations::run`. Also naturally idempotent: a second run (e.g.
/// after a manual meta-row reset) only ever re-examines rows that still
/// contain the literal tag string, and stripping an already-stripped tag is a
/// no-op.
fn migrate_strip_memory_tag_from_derived_reflections(conn: &Connection) -> Result<()> {
    let already_done: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            [STRIP_MEMORY_TAG_FROM_DERIVED_KEY],
            |row| row.get(0),
        )
        .optional()?;
    if already_done.is_some() {
        return Ok(());
    }
    let memory_tag = crate::mcp::tools::EXPLICIT_MEMORY_TAG;
    let mut stmt =
        conn.prepare("SELECT id, tags FROM reflections WHERE tags LIKE '%' || ?1 || '%'")?;
    let rows: Vec<(String, String)> = stmt
        .query_map([memory_tag], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    let mut repaired = 0i64;
    for (id, tags_json) in rows {
        let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
        if !crate::mcp::tools::has_derived_reflection_tag(&tags) {
            continue;
        }
        let stripped: Vec<String> = tags.into_iter().filter(|t| t != memory_tag).collect();
        let stripped_json = serde_json::to_string(&stripped)?;
        conn.execute(
            "UPDATE reflections SET tags = ?1 WHERE id = ?2",
            rusqlite::params![stripped_json, id],
        )?;
        repaired += 1;
    }
    conn.execute(
        "INSERT INTO meta(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![STRIP_MEMORY_TAG_FROM_DERIVED_KEY, repaired.to_string()],
    )?;
    if repaired > 0 {
        tracing::info!(
            repaired,
            "migrate_strip_memory_tag_from_derived_reflections: stripped csr:memory from derived rows"
        );
    }
    Ok(())
}

/// Does `table` carry an index named `index_name`? Answered from
/// `pragma_index_list`, so a column that exists without its index — the exact
/// shape a half-applied migration leaves behind — is *detected*, not assumed
/// complete because the column probe succeeded.
fn has_index(conn: &Connection, table: &str, index_name: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_index_list(?1) WHERE name = ?2",
        rusqlite::params![table, index_name],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Add `narrative_usage.ref_id` and its index, checking each half
/// independently and verifying the result.
///
/// Three properties the previous `let _ = execute_batch(...)` did not have:
///
/// 1. **Errors propagate.** A failed ALTER or CREATE INDEX aborts startup
///    instead of leaving every later `ref_id` insert to fail silently.
/// 2. **The two halves are checked separately.** A database that already has
///    the column but lost the index (partial prerelease schema, interrupted
///    migration) is *repaired*; the old code skipped the whole block the
///    moment the column probe succeeded.
/// 3. **The result is verified.** After the writes, both objects are read
///    back out of the schema; if either is still missing the migration
///    returns an error rather than reporting success.
///
/// Wrapped in a SAVEPOINT so a failure half-way leaves no partial state.
/// SAVEPOINTs nest, so this is safe whether or not the caller already holds a
/// transaction.
fn migrate_narrative_usage_ref_id(conn: &Connection) -> Result<()> {
    const TABLE: &str = "narrative_usage";
    const COLUMN: &str = "ref_id";
    const INDEX: &str = "idx_narrative_usage_ref";

    let column_present = has_column(conn, TABLE, COLUMN)?;
    let index_present = has_index(conn, TABLE, INDEX)?;
    if column_present && index_present {
        return Ok(());
    }

    conn.execute_batch("SAVEPOINT csr_narrative_usage_ref_id")?;
    let applied = (|| -> Result<()> {
        if !column_present {
            add_column(
                conn,
                "narrative_usage",
                "ref_id",
                "ALTER TABLE narrative_usage ADD COLUMN ref_id TEXT",
            )?;
        }
        if !index_present {
            conn.execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_narrative_usage_ref ON narrative_usage(ref_id)",
            )?;
        }
        Ok(())
    })();
    match applied {
        Ok(()) => conn.execute_batch("RELEASE csr_narrative_usage_ref_id")?,
        Err(error) => {
            let _ = conn.execute_batch(
                "ROLLBACK TO csr_narrative_usage_ref_id; RELEASE csr_narrative_usage_ref_id",
            );
            return Err(error);
        }
    }

    // Verify, from the schema, that both halves actually landed. Reporting
    // "migrated" on the strength of a statement that returned Ok is exactly
    // the assumption this finding is about.
    if !has_column(conn, TABLE, COLUMN)? {
        anyhow::bail!("migration failed: narrative_usage.ref_id missing after ALTER TABLE");
    }
    if !has_index(conn, TABLE, INDEX)? {
        anyhow::bail!("migration failed: idx_narrative_usage_ref missing after CREATE INDEX");
    }
    Ok(())
}

/// One-shot backfill (D5): rewrite already-stored worktree-local paths in
/// `code_evolution.file_path` / `code_nodes.file` to their canonical main-repo
/// form. Companion to the `track_code_evolution` fix in `hooks::post_tool_use`
/// (which now canonicalizes on every new write) — this corrects rows written
/// before that fix landed. Never deletes anything. `pub(crate)` so tests can
/// call it directly, independent of the one-shot `meta` gate in `run()`.
pub(crate) fn backfill_worktree_paths(conn: &Connection) -> Result<()> {
    const WORKTREE_MARKER: &str = "%/.claude/worktrees/%";

    // code_evolution: `id` is never derived from `file_path` — plain rewrite,
    // no collision possible, no logical-duplicate concern (append-only ledger).
    {
        let mut stmt =
            conn.prepare("SELECT id, file_path FROM code_evolution WHERE file_path LIKE ?1")?;
        let rows: Vec<(String, String)> = stmt
            .query_map([WORKTREE_MARKER], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);

        for (id, file_path) in rows {
            let canonical =
                crate::extraction::repo_path::canonical_repo_path(std::path::Path::new(&file_path));
            let canonical_str = canonical.to_string_lossy().to_string();
            if canonical_str != file_path {
                conn.execute(
                    "UPDATE code_evolution SET file_path = ?1 WHERE id = ?2",
                    rusqlite::params![canonical_str, id],
                )?;
            }
        }
    }

    // code_nodes is deliberately NOT rewritten here.
    //
    // `id = sha256(repo|file|kind|name)` (extraction::codegraph::node_id), so
    // rewriting `file` without recomputing `id` leaves a row whose stored path
    // disagrees with its own identity. The next extraction of that file mints a
    // second row under the correct canonical id, and `retire_missing_nodes` —
    // which scopes by (project, file) — then sees the migrated legacy row as
    // absent from the observed set and hard-deletes it together with its
    // `code_node_attribution` provenance. The migration would manufacture
    // exactly the data loss the rest of this branch is closing.
    //
    // Re-keying properly would mean rewriting `code_edges.src_id`/`dst_id`,
    // `code_node_attribution.node_id` and `code_node_rank.node_id` in one
    // transaction and merging into any pre-existing canonical row — a real
    // migration, not a release-gate cleanup, and not worth the risk here.
    //
    // Leaving these rows untouched is a no-op against today's behaviour: they
    // already sit under their worktree paths, and retirement never reaches them
    // because it is scoped to the canonical (project, file). The forward fix in
    // `hooks::post_tool_use::update_code_graph` stops new ones being written.
    Ok(())
}

#[cfg(test)]
#[path = "migrations_tests.rs"]
mod tests;
