//! Mirror embeddings written by other processes into the daemon's warm index.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::sync::RwLock;

use crate::search::SearchEngine;
use crate::storage::{embedding_changes, Storage};

const POLL_INTERVAL: Duration = Duration::from_secs(10);
const BATCH_SIZE: usize = 128;

pub(crate) fn spawn(
    storage: Arc<Storage>,
    search: Arc<RwLock<SearchEngine>>,
    shutdown: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run(storage, search, shutdown, POLL_INTERVAL))
}

async fn run(
    storage: Arc<Storage>,
    search: Arc<RwLock<SearchEngine>>,
    shutdown: Arc<AtomicBool>,
    interval: Duration,
) {
    // Replay retained deltas on restart as well: the last disk snapshot can
    // predate an update of an existing ID even when startup backfill finds it.
    let mut cursor = 0;
    let mut generation = None;
    while !shutdown.load(Ordering::SeqCst) {
        let started = Instant::now();
        loop {
            let storage = storage.clone();
            let search = search.clone();
            let shutdown = shutdown.clone();
            let result = tokio::task::spawn_blocking(move || {
                sync_batch(&storage, &search, &shutdown, cursor, generation)
            })
            .await;
            match result {
                Ok(Ok((next, observed_generation, count))) => {
                    cursor = next;
                    generation = Some(observed_generation);
                    if count < BATCH_SIZE || started.elapsed() >= Duration::from_secs(2) {
                        break;
                    }
                }
                result => {
                    tracing::warn!(?result, "embedding delta sync failed; retrying");
                    break;
                }
            }
        }
        // Responsive shutdown without detaching an index-mutating worker.
        let until = Instant::now() + interval;
        while !shutdown.load(Ordering::SeqCst) && Instant::now() < until {
            tokio::time::sleep(Duration::from_millis(100).min(interval)).await;
        }
    }
}

fn sync_batch(
    storage: &Storage,
    search: &RwLock<SearchEngine>,
    shutdown: &AtomicBool,
    mut cursor: i64,
    generation: Option<u64>,
) -> Result<(i64, u64, usize)> {
    let observed_generation = search.blocking_read().generation();
    if generation != Some(observed_generation) {
        // A concurrent compaction/rebuild can swap in a snapshot older than
        // updates we applied. Revisit retained deltas on the new index.
        cursor = 0;
    }
    // SQLite/vector decoding completes BEFORE taking the index lock. No DB
    // IO or embedding inference takes place under that lock.
    let changes =
        storage.with_connection(|conn| embedding_changes::read(conn, cursor, BATCH_SIZE))?;
    let mut count = 0;
    for change in changes {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        // One vector mutation per acquisition, allowing queued prompt readers
        // to run between changes even during a large CLI import.
        let mut index = search.blocking_write();
        if change.reflection {
            if change.invalidate || change.vector.is_none() {
                index.remove_reflection(&change.id);
            }
            if let Some(vector) = change.vector {
                index.insert_reflection(change.id, vector);
            }
        } else {
            if change.invalidate || change.vector.is_none() {
                index.remove_chunk(&change.id);
            }
            if let Some(vector) = change.vector {
                index.insert_chunk(change.id, vector);
            }
        }
        drop(index);
        cursor = change.seq;
        count += 1;
    }
    Ok((cursor, observed_generation, count))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embeddings::EmbeddingEngine;
    use crate::engine::Engine;
    use crate::hooks::{prompt_transport, HookInput};
    use crate::import::ConversationChunk;

    #[test]
    fn rebuild_swap_replays_external_updates_already_applied_to_the_old_index() {
        let storage = Storage::open_memory().unwrap();
        let mut old = vec![0.0; 384];
        old[0] = 1.0;
        let mut updated = vec![0.0; 384];
        updated[1] = 1.0;
        storage.insert_reflection("r", "old", &[], &old).unwrap();
        let search = RwLock::new(SearchEngine::new(100));
        let shutdown = AtomicBool::new(false);
        let (cursor, generation, _) = sync_batch(&storage, &search, &shutdown, 0, None).unwrap();
        // Rebuild takes its snapshot, then an external write is reconciled.
        let mut rebuilding = SearchEngine::new(100);
        rebuilding.insert_reflection("r".into(), old);
        storage
            .insert_reflection("r", "new", &[], &updated)
            .unwrap();
        let (cursor, generation, _) =
            sync_batch(&storage, &search, &shutdown, cursor, Some(generation)).unwrap();
        assert_eq!(
            search
                .blocking_read()
                .search_reflections(&updated, 1, 0.99)
                .len(),
            1
        );
        // Same swap used by Engine::rebuild_search_index; IDs match, but its
        // vector snapshot is stale. A cursor-only poll would return no changes.
        *search.blocking_write() = rebuilding;
        assert!(search
            .blocking_read()
            .search_reflections(&updated, 1, 0.99)
            .is_empty());
        let (_, new_generation, count) =
            sync_batch(&storage, &search, &shutdown, cursor, Some(generation)).unwrap();
        assert_ne!(new_generation, generation);
        assert_eq!(count, 1);
        assert_eq!(
            search
                .blocking_read()
                .search_reflections(&updated, 1, 0.99)
                .len(),
            1
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // Serialize rendering policy environment.
    async fn external_chunk_and_reflection_reach_prompt_socket_and_deletions_disappear() {
        let _env = crate::daemon::dream_cadence::env_test_guard();
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("csr-engine.db");
        let storage = Arc::new(Storage::open(&db).unwrap());
        let search = Arc::new(RwLock::new(SearchEngine::new(1000)));
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let prompt = "authentication timeout regression in token validation needs a fix";
        let vector = embeddings.embed_single(prompt).unwrap();
        let engine = Arc::new(Engine::from_parts_with_index_dir(
            storage.clone(),
            embeddings,
            search.clone(),
            dir.path().to_path_buf(),
            dir.path().join("index"),
        ));
        // Prime the renderer before measuring eventual retrieval; ONNX cold
        // startup is independently bounded by the client, not this sync test.
        let input = HookInput {
            prompt: Some(prompt.into()),
            cwd: Some(dir.path().to_string_lossy().into()),
            session_id: Some("current".into()),
            ..Default::default()
        };
        crate::hooks::prompt_submit::render(&input, &engine, dir.path())
            .await
            .unwrap();
        let shutdown = Arc::new(AtomicBool::new(false));
        let server = prompt_transport::spawn_for_daemon(engine, shutdown.clone());
        let sync = spawn(storage.clone(), search.clone(), shutdown.clone());
        tokio::time::timeout(Duration::from_secs(2), async {
            while !prompt_transport::socket_path(dir.path()).exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        // Separate connection, opened after the serving engine is alive. The
        // writer never receives a reference to the daemon's SearchEngine.
        let writer = Storage::open(&db).unwrap();
        let project =
            crate::search::cross_project::resolve_project_from_cwd(&dir.path().to_string_lossy())
                .unwrap();
        let chunk_text =
            "Authentication timeout was fixed by rotating the token signing key before validation.";
        let reflection_text =
            "Token validation regression was fixed by refreshing expired credentials before retry.";
        writer
            .insert_chunk(
                &ConversationChunk {
                    id: "external-chunk".into(),
                    conversation_id: "previous".into(),
                    project_name: project,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    content: chunk_text.into(),
                    message_count: 4,
                    summary: None,
                    author: crate::provenance::Speaker::User,
                    seq: 0,
                    is_sidechain: false,
                },
                &vector,
            )
            .unwrap();
        writer
            .insert_reflection(
                "external-reflection",
                reflection_text,
                &["solution".into()],
                &vector,
            )
            .unwrap();
        let started = Instant::now();
        let injection = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let Some(text) = prompt_transport::request(&db, dir.path(), input.clone()).await
                {
                    if text.contains(chunk_text) && text.contains(reflection_text) {
                        break text;
                    }
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("both external memories must reach prompt path without restart");
        assert!(injection.contains(chunk_text));
        eprintln!(
            "external memories visible via prompt in {}ms",
            started.elapsed().as_millis()
        );
        writer
            .with_connection(|conn| {
                conn.execute(
                    "DELETE FROM chunk_embeddings WHERE chunk_id='external-chunk'",
                    [],
                )?;
                conn.execute(
                    "DELETE FROM reflection_embeddings WHERE reflection_id='external-reflection'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let index = search.read().await;
                if !index.has_chunk("external-chunk")
                    && !index.has_reflection("external-reflection")
                {
                    break;
                }
                drop(index);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("external deletions must blank both index mappings");
        let after_delete = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(text) = prompt_transport::request(&db, dir.path(), input.clone()).await
                {
                    break text;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap();
        assert!(!after_delete.contains(chunk_text));
        assert!(!after_delete.contains(reflection_text));
        shutdown.store(true, Ordering::SeqCst);
        sync.await.unwrap();
        server.await.unwrap();
    }
}
