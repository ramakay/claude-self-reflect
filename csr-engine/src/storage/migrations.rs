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
    let has_summary_col: bool = conn
        .prepare("SELECT summary FROM chunks LIMIT 0")
        .is_ok();
    if !has_summary_col {
        let _ = conn.execute_batch("ALTER TABLE chunks ADD COLUMN summary TEXT;");
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

    Ok(())
}
