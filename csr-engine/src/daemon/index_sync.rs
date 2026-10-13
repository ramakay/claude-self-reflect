//! Mirror embeddings written by other processes into the daemon's warm index.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::sync::{Notify, RwLock};

use crate::search::SearchEngine;
use crate::storage::{embedding_changes, Storage};

const POLL_INTERVAL: Duration = Duration::from_secs(10);
const BATCH_SIZE: usize = 128;

/// Start the loop. Public for `examples/daemon_stall_probe.rs`, which runs it
/// beside the real watcher and prompt socket; the daemon is its owner.
pub fn spawn(
    storage: Arc<Storage>,
    search: Arc<RwLock<SearchEngine>>,
    shutdown: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    spawn_with_nudge(storage, search, shutdown, Arc::new(Notify::new()))
}

/// [`spawn`], also polling as soon as `nudge` is notified — a proxied MCP
/// server that just stored a reflection asks for it over the vector socket.
pub fn spawn_with_nudge(
    storage: Arc<Storage>,
    search: Arc<RwLock<SearchEngine>>,
    shutdown: Arc<AtomicBool>,
    nudge: Arc<Notify>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run(storage, search, shutdown, POLL_INTERVAL, nudge))
}

async fn run(
    storage: Arc<Storage>,
    search: Arc<RwLock<SearchEngine>>,
    shutdown: Arc<AtomicBool>,
    interval: Duration,
    nudge: Arc<Notify>,
) {
    // Resume after the loaded generation's change cursor (see
    // `SearchEngine::change_cursor`): the startup id backfill cannot see an
    // update of an id the generation already holds, the change log can.
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
            tokio::select! {
                _ = nudge.notified() => break,
                _ = tokio::time::sleep(Duration::from_millis(100).min(interval)) => {}
            }
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
    let (observed_generation, engine_cursor) = {
        let index = search.blocking_read();
        (index.generation(), index.change_cursor())
    };
    if generation != Some(observed_generation) {
        // A load, compaction or remap swapped in another engine. It holds
        // every change up to its own cursor (manifest, or the sequence a
        // rebuild captured before streaming); replay only what follows.
        // Replaying from 0 re-applied every retained update as a remove
        // plus insert, minting one tombstone per updated id on every
        // daemon start and swap.
        cursor = engine_cursor;
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
        // to run between changes even during a large CLI import. While a
        // flush is writing the index files under the read lock, waiting for
        // the write lock would queue every prompt render behind this writer
        // for the rest of the dump (seconds): yield instead, keep the cursor
        // at the last change applied, and resume at the next poll.
        let mut index = match search.try_write() {
            Ok(index) => index,
            Err(_) if crate::search::publish_in_progress() => break,
            Err(_) => search.blocking_write(),
        };
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
        if change.seq > index.change_cursor() {
            index.set_change_cursor(change.seq);
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

    /// A deterministic pseudo-random direction per seed. Orthogonal axes
    /// would leave HNSW's greedy walk with nothing to follow.
    fn axis(seed: usize) -> Vec<f32> {
        let mut state = (seed as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        (0..384)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state % 2001) as f32 / 1000.0 - 1.0
            })
            .collect()
    }

    fn chunk(id: &str) -> ConversationChunk {
        ConversationChunk {
            id: id.into(),
            conversation_id: "conv".into(),
            project_name: "proj".into(),
            timestamp: "2026-10-12T00:00:00Z".into(),
            content: id.into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        }
    }

    #[test]
    fn sync_resumes_after_the_engine_cursor_without_minting_tombstones() {
        let storage = Storage::open_memory().unwrap();
        storage.insert_chunk(&chunk("a"), &axis(0)).unwrap();
        // An update before the rebuild: retained as an invalidating change.
        storage.insert_chunk(&chunk("a"), &axis(1)).unwrap();
        let (engine, _, _) = crate::engine::rebuild_search_index_streaming(&storage, 100).unwrap();
        assert!(engine.change_cursor() > 0);
        let search = RwLock::new(engine);
        let shutdown = AtomicBool::new(false);
        let (cursor, generation, count) =
            sync_batch(&storage, &search, &shutdown, 0, None).unwrap();
        assert_eq!(count, 0, "the rebuild already holds that update");
        assert_eq!(search.blocking_read().chunk_tombstone_count(), 0);

        storage.insert_chunk(&chunk("a"), &axis(2)).unwrap();
        let (_, _, count) =
            sync_batch(&storage, &search, &shutdown, cursor, Some(generation)).unwrap();
        assert_eq!(count, 1);
        let index = search.blocking_read();
        assert_eq!(index.chunk_tombstone_count(), 1);
        assert_eq!(index.search_chunks(&axis(2), 1, 0.99)[0].id, "a");
        assert_eq!(
            index.change_cursor(),
            storage.latest_embedding_change_seq().unwrap()
        );
    }

    /// The dump budget skips dumps, so a restart must recover everything the
    /// skipped generation lacks: new ids from the startup backfill, a new
    /// vector for an id it already holds from the change log after its
    /// cursor, and nothing replayed from before that cursor.
    #[test]
    fn a_skipped_dump_loses_no_vector_on_restart() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("csr-engine.db");
        let projects = dir.path().join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        let storage = Storage::open(&db).unwrap();
        for i in 0..300 {
            storage
                .insert_chunk(&chunk(&format!("c{i}")), &axis(i))
                .unwrap();
        }
        // An update that predates the dump: replaying it would mint a tombstone.
        storage.insert_chunk(&chunk("c1"), &axis(1001)).unwrap();
        // First start: no cache, so the engine rebuilds and dumps.
        drop(Engine::new(&db, &projects).unwrap());

        // A daemon that kept serving without dumping: 20 new ids, and a new
        // vector for one the generation holds.
        for i in 300..320 {
            storage
                .insert_chunk(&chunk(&format!("c{i}")), &axis(i))
                .unwrap();
        }
        storage.insert_chunk(&chunk("c5"), &axis(1050)).unwrap();

        let restarted = Engine::new(&db, &projects).unwrap();
        assert_eq!(restarted.startup_backfill(), (20, 0));
        let shutdown = AtomicBool::new(false);
        let search = restarted.search().clone();
        let (_, _, applied) = sync_batch(&storage, &search, &shutdown, 0, None).unwrap();
        // c5's update plus the 20 inserts (already present, so no-ops).
        assert_eq!(applied, 21);
        let index = search.blocking_read();
        for i in 0..320 {
            assert!(index.has_chunk(&format!("c{i}")), "c{i}");
        }
        assert_eq!(index.search_chunks(&axis(1050), 1, 0.99)[0].id, "c5");
        assert_eq!(index.search_chunks(&axis(1001), 1, 0.99)[0].id, "c1");
        assert_eq!(
            index.chunk_tombstone_count(),
            1,
            "only c5's new vector; c1's older update is not replayed"
        );
    }

    #[test]
    fn a_change_yields_rather_than_queue_behind_a_publish() {
        let storage = Storage::open_memory().unwrap();
        storage
            .insert_reflection("r", "x", &[], &[0.1; 384])
            .unwrap();
        let search = RwLock::new(SearchEngine::new(100));
        let shutdown = AtomicBool::new(false);
        // A publish is writing the index files under the read lock.
        let reader = search.try_read().unwrap();
        let publishing = crate::search::hold_publishing_for_test();
        let (cursor, generation, count) =
            sync_batch(&storage, &search, &shutdown, 0, None).unwrap();
        assert_eq!(
            (cursor, count),
            (0, 0),
            "nothing applied, cursor kept, no wait"
        );
        drop(reader);
        drop(publishing);
        let (_, _, count) =
            sync_batch(&storage, &search, &shutdown, cursor, Some(generation)).unwrap();
        assert_eq!(count, 1);
        assert!(search.try_read().unwrap().has_reflection("r"));
    }
}
