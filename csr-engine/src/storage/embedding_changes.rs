//! Cross-process embedding changes. Triggers include older clients and raw SQL
//! writers; a rowid watermark on the embedding tables would miss deletes/upserts.
use anyhow::Result;
use rusqlite::Connection;

/// Keep the latest change per ID, not an unbounded event history. AUTOINCREMENT
/// survives REPLACE/deletion and makes polling proportional to changed IDs.
/// Entries are retained across restarts: pruning before a matching HNSW dump
/// could resurrect an old vector after an update/delete-and-reinsert.
pub(super) fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS embedding_changes (
            seq INTEGER PRIMARY KEY AUTOINCREMENT,
            kind INTEGER NOT NULL CHECK(kind IN (0, 1)),
            id TEXT NOT NULL,
            invalidate INTEGER NOT NULL,
            UNIQUE(kind, id)
        );",
    )?;
    for (kind, table, id) in [
        (0, "chunk_embeddings", "chunk_id"),
        (1, "reflection_embeddings", "reflection_id"),
    ] {
        // Explicit UPSERT is required: an outer embedding UPSERT's conflict
        // policy can override INSERT OR REPLACE inside a trigger. excluded.seq
        // is the newly allocated AUTOINCREMENT value, including on conflict.
        conn.execute_batch(&format!(
            "CREATE TRIGGER IF NOT EXISTS {table}_change_insert BEFORE INSERT ON {table} BEGIN
                INSERT INTO embedding_changes(kind,id,invalidate)
                VALUES({kind},NEW.{id},EXISTS(SELECT 1 FROM embedding_changes WHERE kind={kind} AND id=NEW.{id}) OR EXISTS(SELECT 1 FROM {table} WHERE {id}=NEW.{id}))
                ON CONFLICT(kind,id) DO UPDATE SET seq=excluded.seq,invalidate=excluded.invalidate;
             END;
             CREATE TRIGGER IF NOT EXISTS {table}_change_update AFTER UPDATE ON {table} BEGIN
                INSERT INTO embedding_changes(kind,id,invalidate) VALUES({kind},OLD.{id},1)
                ON CONFLICT(kind,id) DO UPDATE SET seq=excluded.seq,invalidate=1;
                INSERT INTO embedding_changes(kind,id,invalidate) VALUES({kind},NEW.{id},1)
                ON CONFLICT(kind,id) DO UPDATE SET seq=excluded.seq,invalidate=1;
             END;
             CREATE TRIGGER IF NOT EXISTS {table}_change_delete AFTER DELETE ON {table} BEGIN
                INSERT INTO embedding_changes(kind,id,invalidate) VALUES({kind},OLD.{id},1)
                ON CONFLICT(kind,id) DO UPDATE SET seq=excluded.seq,invalidate=1;
             END;"
        ))?;
    }
    Ok(())
}

pub(crate) struct Change {
    pub seq: i64,
    pub reflection: bool,
    pub id: String,
    pub invalidate: bool,
    pub vector: Option<Vec<f32>>,
}

/// One SELECT supplies a consistent snapshot of changes AND their current
/// vectors. A concurrent commit gets a higher sequence and is read next time.
pub(crate) fn read(conn: &Connection, after: i64, limit: usize) -> Result<Vec<Change>> {
    let mut stmt = conn.prepare(
        "SELECT d.seq,d.kind,d.id,d.invalidate,
                CASE d.kind WHEN 0 THEN c.embedding ELSE r.embedding END
         FROM embedding_changes d
         LEFT JOIN chunk_embeddings c ON d.kind=0 AND c.chunk_id=d.id
         LEFT JOIN reflection_embeddings r ON d.kind=1 AND r.reflection_id=d.id
         WHERE d.seq>?1 ORDER BY d.seq LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![after, limit as i64], |row| {
        let bytes: Option<Vec<u8>> = row.get(4)?;
        Ok(Change {
            seq: row.get(0)?,
            reflection: row.get::<_, i64>(1)? == 1,
            id: row.get(2)?,
            invalidate: row.get::<_, i64>(3)? != 0,
            vector: bytes.map(|bytes| {
                bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| f32::from_le_bytes(*b))
                    .collect()
            }),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Sequence of the newest retained change (0 when none). A rebuild that
/// captures this before it streams vectors from SQLite holds the effect of
/// every change up to it, so the index sync may resume after it.
pub(crate) fn latest_seq(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COALESCE(MAX(seq), 0) FROM embedding_changes",
        [],
        |row| row.get(0),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Storage;

    #[test]
    fn embedding_upsert_keeps_one_change_and_advances_sequence() {
        let storage = Storage::open_memory().unwrap();
        storage.with_connection(|conn| {
            conn.execute("INSERT INTO chunks(id,conversation_id,project_name,timestamp,content,message_count) VALUES('c','conv','project','2026-09-26','content',1)",[])?;
            let mut previous = 0;
            for i in 0..3 {
                conn.execute("INSERT INTO chunk_embeddings(chunk_id,embedding) VALUES('c',?1) ON CONFLICT(chunk_id) DO UPDATE SET embedding=excluded.embedding",[vec![0u8;1536]])?;
                let changes = read(conn,previous,10)?;
                assert_eq!(changes.len(),1);
                assert!(changes[0].seq>previous);
                assert_eq!(changes[0].invalidate,i>0);
                previous=changes[0].seq;
                assert_eq!(read(conn,0,10)?.len(),1);
            }
            Ok(())
        }).unwrap();
    }

    #[test]
    fn first_replace_of_preexisting_vector_is_detected_without_recursive_triggers() {
        let storage = Storage::open_memory().unwrap();
        storage
            .insert_reflection("legacy", "content", &[], &[1.0; 384])
            .unwrap();
        storage.with_connection(|conn| {
            // Simulate a row predating journal migration and a raw SQL writer.
            conn.execute("DELETE FROM embedding_changes",[])?;
            conn.execute_batch("PRAGMA recursive_triggers=OFF")?;
            conn.execute("INSERT OR REPLACE INTO reflection_embeddings(reflection_id,embedding) VALUES ('legacy',?1)",[vec![0u8;1536]])?;
            let changes = read(conn,0,10)?;
            assert_eq!(changes.len(),1);
            assert!(changes[0].invalidate);
            Ok(())
        }).unwrap();
    }

    #[test]
    fn external_updates_deletes_and_reused_ids_have_monotonic_coalesced_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let serving = Storage::open(&path).unwrap();
        let external = Storage::open(&path).unwrap();
        external
            .insert_reflection("r", "first", &[], &[1.0; 384])
            .unwrap();
        let first = serving.with_connection(|conn| read(conn, 0, 10)).unwrap();
        assert_eq!(first.len(), 1);
        assert!(!first[0].invalidate);
        let cursor = first[0].seq;
        external
            .with_connection(|conn| {
                conn.execute(
                    "UPDATE reflection_embeddings SET embedding=?1 WHERE reflection_id='r'",
                    [vec![0u8; 1536]],
                )?;
                Ok(())
            })
            .unwrap();
        let next = serving
            .with_connection(|conn| read(conn, cursor, 10))
            .unwrap();
        assert_eq!(next.len(), 1);
        assert!(next[0].invalidate);
        assert_eq!(next[0].vector.as_ref().unwrap(), &vec![0.0; 384]);
        external
            .with_connection(|conn| {
                conn.execute(
                    "DELETE FROM reflection_embeddings WHERE reflection_id='r'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        let deleted = serving
            .with_connection(|conn| read(conn, next[0].seq, 10))
            .unwrap();
        assert!(deleted[0].vector.is_none());
        external
            .insert_reflection("r", "recreated", &[], &[0.5; 384])
            .unwrap();
        let recreated = serving
            .with_connection(|conn| read(conn, deleted[0].seq, 10))
            .unwrap();
        assert!(recreated[0].invalidate);
        assert_eq!(recreated[0].vector.as_ref().unwrap(), &vec![0.5; 384]);
        assert_eq!(
            serving
                .with_connection(|conn| read(conn, 0, 10))
                .unwrap()
                .len(),
            1
        );
    }
}
