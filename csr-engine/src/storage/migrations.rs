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
        DROP INDEX IF EXISTS idx_witness_verdicts_identity;",
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
}
