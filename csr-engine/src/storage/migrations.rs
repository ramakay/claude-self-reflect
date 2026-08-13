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
            file_mtime TEXT
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
            id          TEXT PRIMARY KEY,          -- sha1(repo|file|kind|name)
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
         CREATE TABLE IF NOT EXISTS ratification_scores (
            conversation_id TEXT PRIMARY KEY,
            score REAL NOT NULL,
            acts_json TEXT NOT NULL,
            ledger_refs TEXT,
            extractor_version TEXT NOT NULL,
            extracted_at INTEGER NOT NULL
         );",
    )?;

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

    finish_chunks_fts_compaction(conn)?;

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
}
