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
    CREATE TRIGGER chunks_fts_au AFTER UPDATE ON chunks BEGIN
        INSERT INTO chunks_fts(chunks_fts, rowid, content)
        VALUES ('delete', old.rowid, old.content);
        INSERT INTO chunks_fts(rowid, content) VALUES (new.rowid, new.content);
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

/// Run all database migrations.
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
            csr_hook_wrappers_scrubbed INTEGER NOT NULL DEFAULT 0
        );

        CREATE INDEX IF NOT EXISTS idx_import_conversation_id
            ON import_state(conversation_id);
        ",
    )?;

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
            let _ = conn.execute_batch(
                "ALTER TABLE code_edges ADD COLUMN src_file TEXT NOT NULL DEFAULT '';",
            );
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
            let _ = conn
                .execute_batch("ALTER TABLE code_edges ADD COLUMN callee_kind TEXT DEFAULT '';");
        }
        let has_boundary: bool = conn
            .prepare("SELECT boundary FROM code_edges LIMIT 0")
            .is_ok();
        if !has_boundary {
            let _ =
                conn.execute_batch("ALTER TABLE code_edges ADD COLUMN boundary TEXT DEFAULT '';");
        }
        let has_evidence: bool = conn
            .prepare("SELECT evidence FROM code_edges LIMIT 0")
            .is_ok();
        if !has_evidence {
            let _ =
                conn.execute_batch("ALTER TABLE code_edges ADD COLUMN evidence TEXT DEFAULT '';");
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
            let _ = conn.execute_batch(
                "ALTER TABLE code_edges ADD COLUMN src_content_hash TEXT DEFAULT '';",
            );
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
            let _ = conn.execute_batch("ALTER TABLE code_nodes ADD COLUMN repo_root TEXT;");
        }
        let has_last_chunk_id: bool = conn
            .prepare("SELECT last_chunk_id FROM code_nodes LIMIT 0")
            .is_ok();
        if !has_last_chunk_id {
            let _ = conn.execute_batch("ALTER TABLE code_nodes ADD COLUMN last_chunk_id TEXT;");
        }
        let _ = conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_code_nodes_repo_root ON code_nodes(repo_root);",
        );

        let has_repo_root_evolution: bool = conn
            .prepare("SELECT repo_root FROM code_evolution LIMIT 0")
            .is_ok();
        if !has_repo_root_evolution {
            let _ = conn.execute_batch("ALTER TABLE code_evolution ADD COLUMN repo_root TEXT;");
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
            let _ = conn.execute_batch(
                "ALTER TABLE code_graph_file_state ADD COLUMN ast_status TEXT NOT NULL DEFAULT 'supported';",
            );
        }
        let _ = conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_code_graph_file_state_ast_status ON code_graph_file_state(ast_status);",
        );
    }

    // Migration: add project_name to code_evolution if missing (v9 cross-project fix)
    {
        let has_project_col: bool = conn
            .prepare("SELECT project_name FROM code_evolution LIMIT 0")
            .is_ok();
        if !has_project_col {
            let _ = conn.execute_batch(
                "ALTER TABLE code_evolution ADD COLUMN project_name TEXT DEFAULT '';",
            );
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
        let _ = conn.execute_batch(
            "ALTER TABLE import_state ADD COLUMN conversation_id TEXT;
             CREATE INDEX IF NOT EXISTS idx_import_conversation_id ON import_state(conversation_id);",
        );
    }

    // Per-file cumulative suppression totals make the global counters idempotent
    // across the full-file reparses used by incremental transcript imports.
    // Legacy rows stay NULL until their first reparse establishes a baseline;
    // fresh databases use the NOT NULL zero defaults in CREATE TABLE above.
    let has_tool_suppression_col = conn
        .prepare("SELECT csr_tool_blocks_suppressed FROM import_state LIMIT 0")
        .is_ok();
    if !has_tool_suppression_col {
        conn.execute_batch(
            "ALTER TABLE import_state ADD COLUMN csr_tool_blocks_suppressed INTEGER;",
        )?;
    }
    let has_wrapper_suppression_col = conn
        .prepare("SELECT csr_hook_wrappers_scrubbed FROM import_state LIMIT 0")
        .is_ok();
    if !has_wrapper_suppression_col {
        conn.execute_batch(
            "ALTER TABLE import_state ADD COLUMN csr_hook_wrappers_scrubbed INTEGER;",
        )?;
    }

    // Migration: add summary column to chunks table if missing (for timeline display)
    let has_summary_col: bool = conn.prepare("SELECT summary FROM chunks LIMIT 0").is_ok();
    if !has_summary_col {
        let _ = conn.execute_batch("ALTER TABLE chunks ADD COLUMN summary TEXT;");
    }

    // Migration: add seq + is_sidechain columns to chunks table (Saga Phase 1 WS1).
    // seq is nullable — old rows stay NULL until backfilled; is_sidechain defaults 0
    // (never sidechain) until backfilled. Both additive, no table rebuild.
    let has_seq_col: bool = conn.prepare("SELECT seq FROM chunks LIMIT 0").is_ok();
    if !has_seq_col {
        let _ = conn.execute_batch("ALTER TABLE chunks ADD COLUMN seq INTEGER;");
    }
    let has_sidechain_col: bool = conn
        .prepare("SELECT is_sidechain FROM chunks LIMIT 0")
        .is_ok();
    if !has_sidechain_col {
        let _ = conn.execute_batch(
            "ALTER TABLE chunks ADD COLUMN is_sidechain INTEGER NOT NULL DEFAULT 0;",
        );
    }

    // Migration: chunk source dimension (v9.4 multi-source corpus). Pure additive
    // ALTER, O(1) on multi-GB DBs. Deliberately NO index: two-value column with no
    // consuming query yet (Codex adversarial review — a CREATE INDEX here would scan
    // the full chunks table during Storage::open on production DBs).
    let has_source_col: bool = conn.prepare("SELECT source FROM chunks LIMIT 0").is_ok();
    if !has_source_col {
        let _ = conn.execute_batch(
            "ALTER TABLE chunks ADD COLUMN source TEXT NOT NULL DEFAULT 'conversation';",
        );
    }

    // The FTS migration records its durable compaction state in meta. Create
    // this small table before the larger metadata block below so the schema
    // replacement and its pending marker can commit atomically.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    )?;
    migrate_chunks_fts(conn)?;

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

    // Migration: journal_headlines gained `description` after first ship on
    // 2026-08-10; DBs created between the two shapes lack the column. Same
    // idempotent ALTER-guard pattern as the code_edges columns above.
    {
        let has_description: bool = conn
            .prepare("SELECT description FROM journal_headlines LIMIT 0")
            .is_ok();
        if !has_description {
            let _ = conn.execute_batch(
                "ALTER TABLE journal_headlines ADD COLUMN description TEXT NOT NULL DEFAULT '';",
            );
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
            source TEXT NOT NULL DEFAULT 'agent',
            created_at TEXT DEFAULT (datetime('now'))
         );
         CREATE INDEX IF NOT EXISTS idx_resolution_chunk ON resolution_ledger(chunk_id, id);",
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
        CREATE INDEX IF NOT EXISTS idx_witness_ledger_lookup ON witness_ledger(project, file, symbol);",
    )?;

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
    // `anchor_obsolete`/`anchor_reinstated`. `receipt_oid` is the commit proving
    // the verdict (the successor's `at_oid` for `superseded_by`, else the HEAD
    // oid observed when the dream cycle ran). `observed_head_oid` is NOT NULL —
    // every event is anchored to the HEAD commit the dream cycle that minted it
    // saw (per-repo HEAD when a run visits multiple repos), never wall-clock
    // time.
    //
    // Idempotency is APP-SIDE ONLY (`witness_verdicts::insert_verdict_if_changed`):
    // before inserting, the writer reads the LATEST event for that witness and
    // skips iff the candidate is identical in (verdict, successor_witness_id,
    // receipt_oid, observed_head_oid). There is deliberately NO UNIQUE identity
    // index on this table: event history legitimately re-visits earlier states
    // (B -> A -> B: superseded, reinstated, superseded again with the exact
    // same fields as the first event) and a UNIQUE index would silently swallow
    // the third event via `INSERT OR IGNORE`, freezing the witness's state at
    // "reinstated" forever. Only the LATEST event per witness matters, so
    // "skip iff identical to latest" is the whole idempotency contract.
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
            category TEXT NOT NULL CHECK (category IN ('unfinished','strategy')),
            subject_key TEXT,
            revision_hash TEXT NOT NULL,
            prose TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            status TEXT NOT NULL DEFAULT 'open'
        );
        CREATE INDEX IF NOT EXISTS idx_dreams_v1_lookup
            ON dreams_v1(project, category, revision_hash);",
    )?;

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
        conn.execute_batch("ALTER TABLE rerank_models ADD COLUMN curated_baseline_score REAL;")?;
    }
    if !has_column(conn, "rerank_models", "curated_trained_score")? {
        conn.execute_batch("ALTER TABLE rerank_models ADD COLUMN curated_trained_score REAL;")?;
    }
    if !has_column(conn, "rerank_models", "curated_veto_epsilon")? {
        conn.execute_batch("ALTER TABLE rerank_models ADD COLUMN curated_veto_epsilon REAL;")?;
    }
    if !has_column(conn, "rerank_models", "curated_case_count")? {
        conn.execute_batch(
            "ALTER TABLE rerank_models ADD COLUMN curated_case_count INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    if !has_column(conn, "rerank_gate_clusters", "distinct_session_count")? {
        conn.execute_batch(
            "ALTER TABLE rerank_gate_clusters
             ADD COLUMN distinct_session_count INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    conn.execute(
        "UPDATE rerank_models SET curated_veto_epsilon = ?1
         WHERE curated_veto_epsilon IS NULL",
        [super::trained_rerank::CURATED_VETO_EPSILON],
    )?;

    finish_chunks_fts_compaction(conn)?;

    Ok(())
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
            // Deliberately NOT `IF NOT EXISTS` (SQLite has no such form for
            // ADD COLUMN): the pragma above already established absence, and
            // a duplicate-column error here means the schema moved under us
            // and must surface.
            conn.execute_batch("ALTER TABLE narrative_usage ADD COLUMN ref_id TEXT")?;
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
mod tests {
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

        run(&conn)
            .expect_err("missing external content column must fail after the transactional DROP");

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
            conn.prepare(
                "SELECT project, file, name, kind, lang, scanned_at FROM repo_defs LIMIT 0"
            )
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
}
