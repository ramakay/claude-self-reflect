//! `Engine::new` must not block on `index.lock` for its opportunistic
//! stale-file cleanup.
//!
//! Every hook is a fresh process that loads the on-disk HNSW cache under a
//! shared `index.lock`. After the load, `Engine::new` asked for the exclusive
//! lock just to delete stale generation files, so it waited for every other
//! process that was still loading. BSD flock gives a pending exclusive request
//! no preference over new shared requests, so a burst of hooks from several
//! open sessions stalls each other's startups in a phase no timing line
//! covers, and a `UserPromptSubmit` hook can run past Claude Code's timeout.

use tempfile::TempDir;

/// Seed SQLite with `n` chunk embeddings (ids `seed-0..n`).
fn seed_db(db_path: &std::path::Path, n: usize) {
    let storage = csr_engine::storage::Storage::open(db_path).unwrap();
    for i in 0..n {
        let chunk = csr_engine::import::ConversationChunk {
            id: format!("seed-{i}"),
            conversation_id: "seed-conv".into(),
            project_name: "seed".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            content: format!("seed chunk {i}"),
            message_count: 1,
            summary: None,
            author: csr_engine::provenance::Speaker::ToolResult,
            seq: i,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &[0.1f32; 384]).unwrap();
    }
}

/// Dump a matching `n`-chunk cache so `Engine::new` takes the cached path.
fn dump_seed_index(index_dir: &std::path::Path, n: usize) {
    let mut seed = csr_engine::search::SearchEngine::new(n.max(16));
    for i in 0..n {
        seed.insert_chunk(format!("seed-{i}"), vec![0.1f32; 384]);
    }
    seed.dump_to_disk(index_dir, n, 0).unwrap();
}

#[test]
fn engine_startup_does_not_wait_on_a_shared_index_lock_holder() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("csr-engine.db");
    let index_dir = tmp.path().join("index");
    std::fs::create_dir_all(&index_dir).unwrap();
    seed_db(&db_path, 50);
    dump_seed_index(&index_dir, 50);

    // Stand-in for a concurrent loader. flock locks on separate open file
    // descriptions conflict even within one process.
    let holder = std::fs::File::open(index_dir.join("index.lock")).unwrap();
    fs2::FileExt::lock_shared(&holder).unwrap();

    let (tx, rx) = std::sync::mpsc::channel();
    let projects_dir = tmp.path().to_path_buf();
    std::thread::spawn(move || {
        let ok = csr_engine::engine::Engine::new(&db_path, &projects_dir).is_ok();
        let _ = tx.send(ok);
    });
    let finished = rx.recv_timeout(std::time::Duration::from_secs(20));
    // Release before asserting so a failure does not leave the thread blocked.
    fs2::FileExt::unlock(&holder).unwrap();
    assert_eq!(
        finished,
        Ok(true),
        "Engine::new blocked on index.lock while another reader held it shared"
    );
}

/// The cleanup still runs when nobody else holds the lock.
#[test]
fn engine_startup_still_cleans_stale_generations_when_the_lock_is_free() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("csr-engine.db");
    let index_dir = tmp.path().join("index");
    std::fs::create_dir_all(&index_dir).unwrap();
    seed_db(&db_path, 50);
    dump_seed_index(&index_dir, 50);

    // A numbered generation pair the committed manifest does not reference,
    // as a crashed dump would leave behind.
    let stale_data = index_dir.join("chunks-999999-0.hnsw.data");
    let stale_graph = index_dir.join("chunks-999999-0.hnsw.graph");
    std::fs::write(&stale_data, b"stale").unwrap();
    std::fs::write(&stale_graph, b"stale").unwrap();

    csr_engine::engine::Engine::new(&db_path, tmp.path()).unwrap();

    assert!(
        !stale_data.exists() && !stale_graph.exists(),
        "startup cleanup must still remove unreferenced generations"
    );
}
