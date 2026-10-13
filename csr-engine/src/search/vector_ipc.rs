//! Daemon-served vector search for MCP servers: one process owns the index.
//!
//! Every MCP server (one per Claude Code session) used to load its own HNSW
//! index and embedding model: 1.3-2.2 GB each, nine at a time on the live
//! machine. While the daemon is live it already holds both, warm. A server
//! started then forwards the vector primitives its tools use — query
//! embedding, chunk kNN (optionally restricted to an id set), reflection kNN
//! — over a private Unix socket beside the prompt socket, and keeps reading
//! everything else from SQLite itself. The daemon runs the very same
//! `SearchEngine`/`EmbeddingEngine` calls, so answers are the ones the
//! server would have computed from the same generation.
//!
//! Any failure — no daemon, a daemon that dies mid-session, a timeout, a
//! version or database mismatch — falls back to the in-process path: the
//! server loads the index itself, lazily and once, exactly as a hook does,
//! and loads the embedding model on first use. Nothing is ever published
//! from that fallback; the daemon remains the only writer of generations.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Notify, RwLock, Semaphore};

use super::{SearchEngine, SearchResult};
use crate::embeddings::EmbeddingEngine;

const VERSION: u32 = 1;
/// A project-scoped search carries the scope's chunk ids: 174k ids (~7 MB of
/// JSON) for the largest project live.
const MAX_FRAME: usize = 64 << 20;
const PING_TIMEOUT: Duration = Duration::from_millis(1500);
/// Above the slowest in-process search (a filtered search that escalates to
/// the whole graph) so a busy daemon is not mistaken for a dead one.
const CALL_TIMEOUT: Duration = Duration::from_secs(10);
const NUDGE_TIMEOUT: Duration = Duration::from_millis(500);
/// After a failed call, go straight to the in-process path for this long.
const RETRY_AFTER: Duration = Duration::from_secs(30);
/// Requests the daemon serves at once; more wait for a slot.
const SERVER_SLOTS: usize = 8;

pub fn socket_path(data_dir: &Path) -> PathBuf {
    data_dir.join("hook-ipc-v1").join("vector.sock")
}

#[derive(Serialize, Deserialize)]
struct Request {
    version: u32,
    database: String,
    op: Op,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Op {
    Ping,
    Embed {
        texts: Vec<String>,
    },
    SearchChunks {
        vector: Vec<f32>,
        limit: usize,
        min_score: f32,
        allowed: Option<Vec<String>>,
    },
    SearchReflections {
        vector: Vec<f32>,
        limit: usize,
        min_score: f32,
    },
    /// Wake the daemon's index sync: the caller just wrote an embedding.
    Sync,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
enum Reply {
    Pong,
    Vectors { vectors: Vec<Vec<f32>> },
    Hits { hits: Vec<(String, f32)> },
    Done,
}

#[derive(Serialize, Deserialize)]
struct Response {
    version: u32,
    reply: Reply,
}

/// Whether `<data_dir>/daemon.lock` names a live process. The daemon writes
/// its pid there once it holds the lock. A cheap pre-check only: the socket
/// handshake decides.
pub fn daemon_lock_live(data_dir: &Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(data_dir.join("daemon.lock")) else {
        return false;
    };
    let Ok(pid) = raw.trim().parse::<i32>() else {
        return false;
    };
    pid > 0 && pid_alive(pid)
}

#[cfg(unix)]
fn pid_alive(pid: i32) -> bool {
    // SAFETY: signal 0 only probes for existence and permission.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn pid_alive(_pid: i32) -> bool {
    false
}

/// Blocking client for the daemon's vector socket.
pub struct VectorClient {
    socket: PathBuf,
    database: String,
}

impl VectorClient {
    pub fn for_database(db_path: &Path) -> Option<Self> {
        Some(Self {
            socket: socket_path(db_path.parent()?),
            database: db_path.file_name()?.to_str()?.to_owned(),
        })
    }

    #[cfg(unix)]
    fn call(&self, op: Op, timeout: Duration) -> Result<Reply> {
        let request = serde_json::to_vec(&Request {
            version: VERSION,
            database: self.database.clone(),
            op,
        })?;
        if request.len() > MAX_FRAME {
            bail!("request of {} bytes exceeds the frame cap", request.len());
        }
        let mut stream = std::os::unix::net::UnixStream::connect(&self.socket)
            .with_context(|| format!("connecting {}", self.socket.display()))?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        stream.write_all(&(request.len() as u32).to_be_bytes())?;
        stream.write_all(&request)?;
        let mut header = [0u8; 4];
        stream.read_exact(&mut header).context("reading reply")?;
        let size = u32::from_be_bytes(header) as usize;
        if size > MAX_FRAME {
            bail!("reply of {size} bytes exceeds the frame cap");
        }
        let mut body = vec![0; size];
        stream.read_exact(&mut body).context("reading reply")?;
        let response: Response = serde_json::from_slice(&body)?;
        if response.version != VERSION {
            bail!("daemon speaks vector protocol {}", response.version);
        }
        Ok(response.reply)
    }

    #[cfg(not(unix))]
    fn call(&self, _op: Op, _timeout: Duration) -> Result<Reply> {
        bail!("no vector socket on this platform")
    }

    /// The daemon answers for this database.
    pub fn ping(&self) -> Result<()> {
        match self.call(Op::Ping, PING_TIMEOUT)? {
            Reply::Pong => Ok(()),
            _ => bail!("unexpected reply to ping"),
        }
    }
}

/// Builds the in-process index when the daemon cannot serve.
pub type IndexLoader = Box<dyn Fn() -> Result<SearchEngine> + Send + Sync>;

/// The vector side of a proxied MCP server: the daemon first, the server's
/// own lazily loaded index and model when that fails.
pub struct VectorProxy {
    client: VectorClient,
    loader: IndexLoader,
    local: std::sync::RwLock<Option<SearchEngine>>,
    load_once: std::sync::Mutex<()>,
    remote_down_until: std::sync::Mutex<Option<Instant>>,
    remote_calls: AtomicU64,
    remote_failures: AtomicU64,
}

impl VectorProxy {
    pub fn new(client: VectorClient, loader: IndexLoader) -> Self {
        Self {
            client,
            loader,
            local: std::sync::RwLock::new(None),
            load_once: std::sync::Mutex::new(()),
            remote_down_until: std::sync::Mutex::new(None),
            remote_calls: AtomicU64::new(0),
            remote_failures: AtomicU64::new(0),
        }
    }

    /// Whether the in-process fallback index has been loaded.
    pub fn local_loaded(&self) -> bool {
        self.local.read().is_ok_and(|local| local.is_some())
    }

    /// `(calls answered by the daemon, calls that failed over)`.
    pub fn remote_stats(&self) -> (u64, u64) {
        (
            self.remote_calls.load(Ordering::Relaxed),
            self.remote_failures.load(Ordering::Relaxed),
        )
    }

    fn remote(&self, op: Op) -> Option<Reply> {
        if let Ok(until) = self.remote_down_until.lock() {
            if until.is_some_and(|until| Instant::now() < until) {
                return None;
            }
        }
        match self.client.call(op, CALL_TIMEOUT) {
            Ok(reply) => {
                self.remote_calls.fetch_add(1, Ordering::Relaxed);
                Some(reply)
            }
            Err(error) => {
                self.remote_failures.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    %error,
                    "daemon vector search unavailable; using the in-process index"
                );
                if let Ok(mut until) = self.remote_down_until.lock() {
                    *until = Some(Instant::now() + RETRY_AFTER);
                }
                None
            }
        }
    }

    fn hits(reply: Option<Reply>) -> Option<Vec<SearchResult>> {
        match reply? {
            Reply::Hits { hits } => Some(
                hits.into_iter()
                    .map(|(id, score)| SearchResult { id, score })
                    .collect(),
            ),
            _ => None,
        }
    }

    /// Run `f` on the in-process index, loading it first if needed. A load
    /// takes seconds at the live corpus size and happens once per process.
    fn with_local<T>(&self, empty: T, f: impl FnOnce(&SearchEngine) -> T) -> T {
        if !self.local_loaded() {
            let _once = self.load_once.lock();
            if !self.local_loaded() {
                let started = Instant::now();
                match (self.loader)() {
                    Ok(index) => {
                        if let Ok(mut local) = self.local.write() {
                            *local = Some(index);
                        }
                        tracing::info!(
                            elapsed_ms = started.elapsed().as_millis() as u64,
                            "loaded the index in-process (daemon vector search unavailable)"
                        );
                    }
                    Err(error) => {
                        tracing::warn!(%error, "in-process index load failed");
                        return empty;
                    }
                }
            }
        }
        match self.local.read() {
            Ok(local) => match local.as_ref() {
                Some(index) => f(index),
                None => empty,
            },
            Err(_) => empty,
        }
    }

    pub(crate) fn search_chunks(
        &self,
        vector: &[f32],
        limit: usize,
        min_score: f32,
    ) -> Vec<SearchResult> {
        let op = Op::SearchChunks {
            vector: vector.to_vec(),
            limit,
            min_score,
            allowed: None,
        };
        Self::hits(self.remote(op)).unwrap_or_else(|| {
            self.with_local(Vec::new(), |index| {
                index.search_chunks(vector, limit, min_score)
            })
        })
    }

    pub(crate) fn search_chunks_filtered(
        &self,
        vector: &[f32],
        limit: usize,
        min_score: f32,
        allowed: &HashSet<String>,
    ) -> Vec<SearchResult> {
        let op = Op::SearchChunks {
            vector: vector.to_vec(),
            limit,
            min_score,
            allowed: Some(allowed.iter().cloned().collect()),
        };
        Self::hits(self.remote(op)).unwrap_or_else(|| {
            self.with_local(Vec::new(), |index| {
                index.search_chunks_filtered(vector, limit, min_score, allowed)
            })
        })
    }

    pub(crate) fn search_reflections(
        &self,
        vector: &[f32],
        limit: usize,
        min_score: f32,
    ) -> Vec<SearchResult> {
        let op = Op::SearchReflections {
            vector: vector.to_vec(),
            limit,
            min_score,
        };
        Self::hits(self.remote(op)).unwrap_or_else(|| {
            self.with_local(Vec::new(), |index| {
                index.search_reflections(vector, limit, min_score)
            })
        })
    }

    /// The daemon's embedding of `texts`, or `None` to embed in-process.
    pub(crate) fn embed(&self, texts: &[&str]) -> Option<Vec<Vec<f32>>> {
        let op = Op::Embed {
            texts: texts.iter().map(|text| text.to_string()).collect(),
        };
        match self.remote(op)? {
            Reply::Vectors { vectors } if vectors.len() == texts.len() => Some(vectors),
            _ => None,
        }
    }

    /// The server wrote an embedding to SQLite and would have inserted it
    /// into its own index: apply it to the fallback if loaded, and wake the
    /// daemon's index sync so its index has it now rather than within 10 s.
    pub(crate) fn note_mutation(&self, apply: impl FnOnce(&mut SearchEngine)) {
        if let Ok(mut local) = self.local.write() {
            if let Some(index) = local.as_mut() {
                apply(index);
            }
        }
        let _ = self.client.call(Op::Sync, NUDGE_TIMEOUT);
    }
}

/// Serve the vector socket from the daemon's warm engine until `shutdown`.
/// Never takes the daemon's heavy-work permit: searches only read the index.
pub fn spawn_for_daemon(
    embeddings: Arc<EmbeddingEngine>,
    search: Arc<RwLock<SearchEngine>>,
    db_path: &Path,
    shutdown: Arc<AtomicBool>,
    sync_nudge: Arc<Notify>,
) -> tokio::task::JoinHandle<()> {
    let data_dir = db_path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let database = db_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("csr-engine.db")
        .to_owned();
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            let bound = match crate::hooks::prompt_transport::BoundSocket::bind_named(
                &data_dir,
                "vector.sock",
                "vector.lock",
            ) {
                Ok(bound) => bound,
                Err(error) => {
                    tracing::warn!(%error, "vector socket unavailable");
                    return;
                }
            };
            let slots = Arc::new(Semaphore::new(SERVER_SLOTS));
            while !shutdown.load(Ordering::SeqCst) {
                let accepted = tokio::select! {
                    accepted = bound.listener.accept() => accepted,
                    _ = tokio::time::sleep(Duration::from_millis(100)) => continue,
                };
                let Ok((stream, _)) = accepted else {
                    continue;
                };
                let (embeddings, search) = (embeddings.clone(), search.clone());
                let (database, nudge, slots) =
                    (database.clone(), sync_nudge.clone(), slots.clone());
                tokio::spawn(async move {
                    let _ = tokio::time::timeout(
                        CALL_TIMEOUT,
                        serve_one(stream, embeddings, search, database, nudge, slots),
                    )
                    .await;
                });
            }
        }
        #[cfg(not(unix))]
        let _ = (embeddings, search, data_dir, database, shutdown, sync_nudge);
    })
}

#[cfg(unix)]
async fn serve_one(
    mut stream: tokio::net::UnixStream,
    embeddings: Arc<EmbeddingEngine>,
    search: Arc<RwLock<SearchEngine>>,
    database: String,
    nudge: Arc<Notify>,
    slots: Arc<Semaphore>,
) -> Option<()> {
    let size = stream.read_u32().await.ok()? as usize;
    if size > MAX_FRAME {
        return None;
    }
    let mut body = vec![0; size];
    stream.read_exact(&mut body).await.ok()?;
    let request: Request = serde_json::from_slice(&body).ok()?;
    // A server for another database or protocol gets no answer and falls
    // back; it never gets results from the wrong index.
    if request.version != VERSION || request.database != database {
        return None;
    }
    let _slot = slots.acquire_owned().await.ok()?;
    let reply = tokio::task::spawn_blocking(move || -> Result<Reply> {
        Ok(match request.op {
            Op::Ping => Reply::Pong,
            Op::Embed { texts } => {
                let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
                Reply::Vectors {
                    vectors: embeddings.embed(&refs)?,
                }
            }
            Op::SearchChunks {
                vector,
                limit,
                min_score,
                allowed,
            } => {
                let index = search.blocking_read();
                let hits = match allowed {
                    Some(ids) => {
                        let allowed: HashSet<String> = ids.into_iter().collect();
                        index.search_chunks_filtered(&vector, limit, min_score, &allowed)
                    }
                    None => index.search_chunks(&vector, limit, min_score),
                };
                Reply::Hits {
                    hits: hits.into_iter().map(|hit| (hit.id, hit.score)).collect(),
                }
            }
            Op::SearchReflections {
                vector,
                limit,
                min_score,
            } => Reply::Hits {
                hits: search
                    .blocking_read()
                    .search_reflections(&vector, limit, min_score)
                    .into_iter()
                    .map(|hit| (hit.id, hit.score))
                    .collect(),
            },
            Op::Sync => {
                nudge.notify_one();
                Reply::Done
            }
        })
    })
    .await
    .ok()?
    .ok()?;
    let bytes = serde_json::to_vec(&Response {
        version: VERSION,
        reply,
    })
    .ok()?;
    if bytes.len() > MAX_FRAME {
        return None;
    }
    stream.write_u32(bytes.len() as u32).await.ok()?;
    stream.write_all(&bytes).await.ok()?;
    Some(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use crate::import::ConversationChunk;
    use crate::storage::Storage;

    /// A deterministic pseudo-random 384-dim direction per seed.
    fn direction(seed: usize) -> Vec<f32> {
        let mut state = (seed as u64 + 11).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        (0..384)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state % 2001) as f32 / 1000.0 - 1.0
            })
            .collect()
    }

    const TOPICS: [&str; 6] = [
        "authentication token refresh before retry",
        "HNSW index dump written to disk",
        "embedding batch padded to the longest text",
        "daemon lock and socket handshake",
        "SQLite temp b-tree spilled during grouping",
        "release checklist and npm publish",
    ];

    /// 600 chunks over two projects, past `EXACT_SCAN_THRESHOLD` so HNSW
    /// (not the exact scan) answers, and 40 reflections; then a published
    /// generation, as the daemon leaves it.
    fn seed(dir: &Path) -> (PathBuf, PathBuf) {
        let db = dir.join("csr-engine.db");
        let projects = dir.join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        let storage = Storage::open(&db).unwrap();
        for i in 0..600 {
            storage
                .insert_chunk(
                    &ConversationChunk {
                        id: format!("chunk-{i}"),
                        conversation_id: format!("conv-{}", i % 30),
                        project_name: if i % 2 == 0 { "alpha" } else { "beta" }.into(),
                        timestamp: format!("2026-10-{:02}T00:00:00Z", 1 + i % 11),
                        content: format!("{} (note {i})", TOPICS[i % TOPICS.len()]),
                        message_count: 2,
                        summary: None,
                        author: crate::provenance::Speaker::User,
                        seq: 0,
                        is_sidechain: false,
                    },
                    &direction(i),
                )
                .unwrap();
        }
        for i in 0..40 {
            storage
                .insert_reflection(
                    &format!("reflection-{i}"),
                    TOPICS[i % TOPICS.len()],
                    &["solution".into()],
                    &direction(10_000 + i),
                )
                .unwrap();
        }
        drop(storage);
        // Cache miss: rebuilds from SQLite and publishes the generation.
        drop(Engine::new(&db, &projects).unwrap());
        (db, projects)
    }

    struct TestDaemon {
        shutdown: Arc<AtomicBool>,
        handles: Vec<tokio::task::JoinHandle<()>>,
        search: Arc<RwLock<SearchEngine>>,
    }

    impl TestDaemon {
        async fn start(db: &Path) -> Self {
            let data_dir = db.parent().unwrap();
            let storage = Arc::new(Storage::open(db).unwrap());
            let chunks = storage.count_chunk_embeddings().unwrap();
            let reflections = storage.count_reflection_embeddings().unwrap();
            let index = SearchEngine::load_from_disk(&data_dir.join("index"), chunks, reflections)
                .expect("published generation");
            let search = Arc::new(RwLock::new(index));
            let shutdown = Arc::new(AtomicBool::new(false));
            let nudge = Arc::new(Notify::new());
            let sync = crate::daemon::index_sync::spawn_with_nudge(
                storage,
                search.clone(),
                shutdown.clone(),
                nudge.clone(),
            );
            let server = spawn_for_daemon(
                Arc::new(EmbeddingEngine::new().unwrap()),
                search.clone(),
                db,
                shutdown.clone(),
                nudge,
            );
            tokio::time::timeout(Duration::from_secs(5), async {
                while !socket_path(data_dir).exists() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            std::fs::write(data_dir.join("daemon.lock"), std::process::id().to_string()).unwrap();
            Self {
                shutdown,
                handles: vec![sync, server],
                search,
            }
        }

        async fn stop(self) {
            self.shutdown.store(true, Ordering::SeqCst);
            for handle in self.handles {
                handle.await.unwrap();
            }
        }
    }

    fn same(label: &str, proxied: &[SearchResult], local: &[SearchResult]) {
        let ids = |hits: &[SearchResult]| hits.iter().map(|h| h.id.clone()).collect::<Vec<_>>();
        assert!(!local.is_empty(), "{label}: empty fixture answer");
        assert_eq!(ids(proxied), ids(local), "{label}");
        for (p, l) in proxied.iter().zip(local) {
            assert!(
                (p.score - l.score).abs() < 1e-5,
                "{label}: {} vs {}",
                p.score,
                l.score
            );
        }
    }

    async fn compare_primitives(proxied: &Engine, local: &Engine, tag: &str) {
        let alpha: HashSet<String> = local
            .storage()
            .get_chunk_ids_for_project("alpha")
            .unwrap()
            .into_iter()
            .collect();
        let p = proxied.search().read().await;
        let l = local.search().read().await;
        for q in 0..25 {
            let v = direction(50_000 + q);
            same(
                &format!("{tag} chunks q{q}"),
                &p.search_chunks(&v, 10, -1.0),
                &l.search_chunks(&v, 10, -1.0),
            );
            same(
                &format!("{tag} alpha q{q}"),
                &p.search_chunks_filtered(&v, 10, -1.0, &alpha),
                &l.search_chunks_filtered(&v, 10, -1.0, &alpha),
            );
            same(
                &format!("{tag} reflections q{q}"),
                &p.search_reflections(&v, 10, -1.0),
                &l.search_reflections(&v, 10, -1.0),
            );
        }
    }

    /// Tool output without the timings it reports.
    fn untimed(text: &str) -> String {
        let perf = regex::Regex::new(r"(?s)<perf>.*?</perf>|⚡ PERFORMANCE: \d+ms").unwrap();
        perf.replace_all(text, "").into_owned()
    }

    fn copy_data_dir(from: &Path, to: &Path) {
        std::fs::create_dir_all(to.join("index")).unwrap();
        std::fs::create_dir_all(to.join("projects")).unwrap();
        for entry in std::fs::read_dir(from).unwrap().flatten() {
            if entry.path().is_file()
                && entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("csr-engine.db")
            {
                std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
            }
        }
        for entry in std::fs::read_dir(from.join("index")).unwrap().flatten() {
            std::fs::copy(entry.path(), to.join("index").join(entry.file_name())).unwrap();
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn proxied_answers_equal_the_in_process_index_until_the_daemon_dies() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (db, projects) = seed(tmp.path());
        // The in-process side reads its own copy, so tool calls that record
        // retrievals evolve two identical databases identically.
        let local_dir = tempfile::TempDir::new().unwrap();
        copy_data_dir(tmp.path(), local_dir.path());
        let local = Engine::new_for_hook(
            &local_dir.path().join("csr-engine.db"),
            &local_dir.path().join("projects"),
        )
        .unwrap();

        let daemon = TestDaemon::start(&db).await;
        let proxied = Engine::new_proxied(&db, &projects)
            .unwrap()
            .expect("the daemon answers");
        assert!(proxied.is_proxied());
        let proxy = proxied.search().read().await.proxy().unwrap().clone();

        compare_primitives(&proxied, &local, "daemon").await;
        assert!(!proxy.local_loaded(), "the server never loaded the index");
        assert_eq!(proxy.remote_stats().1, 0, "no call failed over");

        for query in ["authentication token refresh", "index dump to disk"] {
            let (s, e, x) = (proxied.storage(), proxied.embeddings(), proxied.search());
            let (ls, le, lx) = (local.storage(), local.embeddings(), local.search());
            let tools = crate::mcp::tools::reflect_on_past(s, e, x, query, 5, 0.0, Some("all"))
                .await
                .unwrap();
            let expected =
                crate::mcp::tools::reflect_on_past(ls, le, lx, query, 5, 0.0, Some("all"))
                    .await
                    .unwrap();
            assert_eq!(untimed(&tools), untimed(&expected), "{query}");
            assert!(tools.contains("<id>"), "{tools}");
            let tools = crate::mcp::tools::quick_check(s, e, x, query, 0.0, Some("alpha"))
                .await
                .unwrap();
            let expected = crate::mcp::tools::quick_check(ls, le, lx, query, 0.0, Some("alpha"))
                .await
                .unwrap();
            assert_eq!(untimed(&tools), untimed(&expected), "{query}");
        }
        assert!(
            !proxied.embeddings().is_loaded(),
            "queries were embedded by the daemon"
        );
        assert!(!proxy.local_loaded());

        // A reflection stored through the server reaches the daemon's index
        // now (the sync nudge), not at its next 10 s poll.
        crate::mcp::tools::store_reflection_internal(
            proxied.storage(),
            proxied.embeddings(),
            proxied.search(),
            "proxied servers write SQLite and nudge the daemon",
            &["solution".into()],
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while daemon.search.read().await.reflection_count() != 41 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the daemon's index has the reflection well before its 10 s poll");

        // The daemon dies mid-session: the same calls are answered by the
        // index the server now loads itself, and the model it loads itself.
        daemon.stop().await;
        assert!(!socket_path(tmp.path()).exists());
        let proxied_after = Engine::new_proxied(&db, &projects).unwrap();
        assert!(proxied_after.is_none(), "no daemon, no proxy");
        // In-process reference over the same database, which now holds the
        // reflection stored above.
        let fresh_local = Engine::new_for_hook(&db, &projects).unwrap();
        compare_primitives(&proxied, &fresh_local, "fallback").await;
        assert!(proxy.local_loaded(), "the fallback loaded the index once");
        assert!(proxy.remote_stats().1 >= 1);
        let v = proxied
            .embeddings()
            .embed_single("index dump to disk")
            .unwrap();
        assert!(
            proxied.embeddings().is_loaded(),
            "the fallback loaded the model"
        );
        let w = fresh_local
            .embeddings()
            .embed_single("index dump to disk")
            .unwrap();
        assert!(v.iter().zip(&w).all(|(a, b)| (a - b).abs() < 1e-6));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_daemon_means_no_proxy() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (db, projects) = seed(tmp.path());
        assert!(Engine::new_proxied(&db, &projects).unwrap().is_none());
        // A lock naming a live pid is not enough: the socket must answer.
        std::fs::write(
            tmp.path().join("daemon.lock"),
            std::process::id().to_string(),
        )
        .unwrap();
        assert!(Engine::new_proxied(&db, &projects).unwrap().is_none());
        // A socket answering for another database is not enough either.
        let daemon = TestDaemon::start(&db).await;
        let other = tmp.path().join("other.db");
        assert!(Engine::new_proxied(&other, &projects).unwrap().is_none());
        // A dead pid in the lock is not enough.
        std::fs::write(tmp.path().join("daemon.lock"), "999999").unwrap();
        assert!(Engine::new_proxied(&db, &projects).unwrap().is_none());
        daemon.stop().await;
    }
}
