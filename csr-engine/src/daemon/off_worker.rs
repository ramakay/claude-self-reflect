//! Daemon storage calls that wait on the daemon's one connection, kept off the
//! async workers.
//!
//! The daemon's `Storage` is one `Mutex<Connection>`, and blocking-pool jobs
//! hold it for whole statements: the extraction tick's
//! `get_unenriched_conversations` reads every chunk row, 0.6 s warm and about
//! 20 s cold on a 13 GB store. A task that takes the same mutex on a tokio
//! worker parks that worker for the whole wait. The daemon runs 4 workers, and
//! the idle gates alone ask for `last_activity_at` every 10 s from several
//! loops, so a few parked waiters leave the prompt socket's accept loop no
//! worker: answers that start seconds late or never, with no build running.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use tokio::sync::RwLock;

use crate::api::BatchClient;
use crate::embeddings::EmbeddingEngine;
use crate::search::SearchEngine;
use crate::storage::Storage;

/// [`super::dream_cadence::last_activity_at`] on the blocking pool.
pub(super) async fn last_activity(storage: &Arc<Storage>) -> Option<DateTime<Utc>> {
    let storage = storage.clone();
    tokio::task::spawn_blocking(move || super::dream_cadence::last_activity_at(&storage))
        .await
        .ok()
        .flatten()
}

/// One consolidation pass ([`super::run_consolidation`]) off the async workers.
pub(super) async fn consolidation(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
) -> Result<()> {
    let (s, e, i) = (storage.clone(), embeddings.clone(), search.clone());
    crate::runtime::run_off_worker(
        move || async move { super::run_consolidation(&s, &e, &i).await },
    )
    .await?
}

/// One narrator pass ([`super::narrator_loop_inner`]) off the async workers.
#[allow(clippy::too_many_arguments)]
pub(super) async fn narrator_pass(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    client: &Arc<dyn BatchClient>,
    batch_size: usize,
    batch_time_secs: u64,
    poll_interval_secs: u64,
    last_batch_time: &mut tokio::time::Instant,
    shutdown: &Arc<AtomicBool>,
) -> Result<()> {
    let (s, e, i, c, stop) = (
        storage.clone(),
        embeddings.clone(),
        search.clone(),
        client.clone(),
        shutdown.clone(),
    );
    let started = *last_batch_time;
    let (result, last) = crate::runtime::run_off_worker(move || async move {
        let mut last = started;
        let result = super::narrator_loop_inner(
            &s,
            &e,
            &i,
            &c,
            batch_size,
            batch_time_secs,
            poll_interval_secs,
            &mut last,
            &stop,
        )
        .await;
        (result, last)
    })
    .await?;
    *last_batch_time = last;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    /// The idle gate asked while another job holds the daemon connection: on a
    /// one-worker runtime, a task spawned after it still runs on time. With the
    /// check on the worker, that task waited out the whole 800 ms hold.
    #[test]
    fn an_idle_check_behind_a_held_connection_leaves_the_worker_free() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let storage = Arc::new(Storage::open_memory().unwrap());
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let holder = {
            let storage = storage.clone();
            std::thread::spawn(move || {
                storage
                    .with_connection(|_| {
                        held_tx.send(()).unwrap();
                        std::thread::sleep(Duration::from_millis(800));
                        Ok(())
                    })
                    .unwrap();
            })
        };
        held_rx.recv().unwrap();
        let shutdown = Arc::new(AtomicBool::new(false));
        let waited = runtime.block_on(async {
            let gate = {
                let (storage, shutdown) = (storage.clone(), shutdown.clone());
                tokio::spawn(async move {
                    super::super::wait_until_idle_or_shutdown(&storage, &shutdown).await
                })
            };
            let spawned = Instant::now();
            let tick = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                spawned.elapsed()
            });
            let waited = tick.await.unwrap();
            shutdown.store(true, Ordering::SeqCst);
            gate.abort();
            waited
        });
        holder.join().unwrap();
        assert!(waited < Duration::from_millis(400), "{waited:?}");
    }
}
