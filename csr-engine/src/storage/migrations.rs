use anyhow::Result;
use rusqlite::Connection;

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

    // FTS5 for hybrid search — CREATE VIRTUAL TABLE doesn't support IF NOT EXISTS
    // so we check manually
    let has_fts: bool = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='chunks_fts'")?
        .exists([])?;

    if !has_fts {
        conn.execute_batch(
            "CREATE VIRTUAL TABLE chunks_fts USING fts5(content, tokenize='porter unicode61');",
        )?;
    }

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

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
