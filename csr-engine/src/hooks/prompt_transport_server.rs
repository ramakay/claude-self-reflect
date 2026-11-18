//! The daemon end of the prompt socket: one render at a time, each request
//! bounded by the wait its client declared, and one timing line for an answer
//! that is slow or ends past its deadline (`prompt_stages`).

use super::*;
#[cfg(unix)]
use crate::hooks::prompt_stages::{self, AnswerTimes, Outcome};

/// What the daemon keeps back from a client's declared wait: the reply still
/// has to be written, read and acknowledged before the client's own cutoff.
pub(super) const REPLY_MARGIN: Duration = Duration::from_millis(10);

/// A request's deadline after accept: the client's declared wait less
/// [`REPLY_MARGIN`], never above [`PROCESS_BUDGET`]; [`REQUEST_DEADLINE`] for
/// a client that declares none.
fn request_deadline(declared_ms: Option<u64>) -> Duration {
    match declared_ms {
        Some(ms) => Duration::from_millis(ms)
            .min(PROCESS_BUDGET)
            .saturating_sub(REPLY_MARGIN),
        None => REQUEST_DEADLINE,
    }
}

/// Read, on the render connection, what a first render would otherwise read
/// cold: the frame vocabulary (the serve-only daemon runs no refresh loop,
/// and the daemon's own loop starts with the socket) and every project's
/// receipted dream conclusions. Reads only; a failure leaves the render to
/// read them itself.
#[cfg(unix)]
fn warm(render: &Engine) {
    let storage = render.storage();
    let _ = storage.reflection_common_terms();
    let _ = storage.with_connection(crate::storage::dream_delivery::warm_receipted_conclusions);
}

/// `PRAGMA query_only=ON` on a storage handle. Failure leaves it writable.
fn make_query_only(storage: &crate::storage::Storage) {
    let _ = storage.with_connection(|conn| {
        conn.execute_batch("PRAGMA query_only=ON")?;
        Ok(())
    });
}

pub(super) fn spawn(
    engine: Arc<Engine>,
    shutdown: Arc<AtomicBool>,
    read_only: bool,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            let data_dir = engine.index_dir().parent().unwrap_or(Path::new("."));
            let database = engine.storage().with_connection(|conn| {
                Ok(conn
                    .path()
                    .and_then(|path| Path::new(path).file_name())
                    .and_then(|name| name.to_str())
                    .unwrap_or("csr-engine.db")
                    .to_owned())
            });
            let Ok(database) = database else { return };
            // Renders read through their own connection, so a background loop
            // holding the daemon's connection for a long statement cannot park
            // a prompt behind it (see `Storage::open_prompt_lane`). Writes that
            // must not be lost, and may wait, stay on the daemon's connection:
            // `receipts`.
            let receipts = engine.storage().clone();
            if read_only {
                make_query_only(&receipts);
            }
            let render_engine = match engine.storage().open_prompt_lane() {
                Ok(Some(lane)) => Arc::new(Engine::from_parts_with_index_dir(
                    {
                        if read_only {
                            make_query_only(&lane);
                        }
                        Arc::new(lane)
                    },
                    engine.embeddings().clone(),
                    engine.search().clone(),
                    engine.projects_dir().to_path_buf(),
                    engine.index_dir().to_path_buf(),
                )),
                Ok(None) => engine.clone(),
                Err(e) => {
                    tracing::warn!(%e, "prompt lane unavailable, renders share the daemon connection");
                    engine.clone()
                }
            };
            // Ready before the socket exists: a hook finds no socket and fails
            // open at once, where a cold first render would hold the one
            // render slot past the deadlines of the prompts queued behind it.
            let warming = render_engine.clone();
            let _ = tokio::task::spawn_blocking(move || warm(&warming)).await;
            // Off the async workers: a contended lock waits (see `lock_within`).
            let owned_dir = data_dir.to_path_buf();
            let bound = match tokio::task::spawn_blocking(move || BoundSocket::bind(&owned_dir))
                .await
                .unwrap_or_else(|e| Err(std::io::Error::other(e)))
            {
                Ok(bound) => bound,
                Err(e) => {
                    tracing::warn!(%e, "prompt socket unavailable");
                    return;
                }
            };
            // Measure the frame vocabulary (`injection::specificity`) off the
            // prompt path: before the first prompt, then well inside each
            // validity window, so a render only ever reads the cached copy. It
            // runs on the daemon's connection (reads, then one meta write that
            // may wait for another writer) and publishes into the cache the lane
            // shares; the lane's own mutex is never held for that.
            if !read_only {
                let engine = engine.clone();
                let shutdown = shutdown.clone();
                tokio::spawn(async move {
                    while !shutdown.load(Ordering::SeqCst) {
                        let worker = engine.clone();
                        let _ = tokio::task::spawn_blocking(move || {
                            worker.storage().refresh_reflection_common_terms();
                        })
                        .await;
                        tokio::time::sleep(COMMON_TERMS_REFRESH).await;
                    }
                });
            }
            let engine = render_engine;
            let slots = Arc::new(Semaphore::new(1));
            while !shutdown.load(Ordering::SeqCst) {
                let accepted = tokio::select! {
                    accepted = bound.listener.accept() => accepted,
                    _ = tokio::time::sleep(Duration::from_millis(100)) => continue,
                };
                let Ok((stream, _)) = accepted else {
                    continue;
                };
                tokio::spawn(answer(
                    stream,
                    engine.clone(),
                    receipts.clone(),
                    database.clone(),
                    slots.clone(),
                    read_only,
                ));
            }
        }
        #[cfg(not(unix))]
        let _ = (engine, shutdown, read_only);
    })
}

/// A request this daemon may answer: same protocol, database, policy and
/// projects directory.
#[cfg(unix)]
fn accepts(request: &Request, database: &str, engine: &Engine) -> bool {
    request.version == VERSION
        && request.database == database
        && (request.kind != Kind::Prompt || request.policy == policy())
        && request.projects_dir
            == engine
                .projects_dir()
                .canonicalize()
                .unwrap_or_else(|_| engine.projects_dir().to_path_buf())
}

/// Answer one connection. Past its deadline the stream is dropped and the
/// client sees EOF.
#[cfg(unix)]
async fn answer(
    mut stream: tokio::net::UnixStream,
    engine: Arc<Engine>,
    receipts: Arc<crate::storage::Storage>,
    database: String,
    slots: Arc<Semaphore>,
    read_only: bool,
) {
    let accepted = tokio::time::Instant::now();
    let read = tokio::time::timeout_at(accepted + REQUEST_DEADLINE, async {
        let bytes = read_frame(&mut stream).await?;
        let request: Request = serde_json::from_slice(&bytes).ok()?;
        accepts(&request, &database, &engine).then_some(request)
    })
    .await;
    let Ok(Some(request)) = read else { return };
    let deadline = accepted + request_deadline(request.wait_ms);
    let mut times = AnswerTimes {
        deadline: deadline - accepted,
        ..AnswerTimes::default()
    };
    // One render at a time, but a request waits its turn instead of being
    // dropped.
    let slot_started = tokio::time::Instant::now();
    let slot = match tokio::time::timeout_at(deadline, slots.acquire_owned()).await {
        Ok(Ok(slot)) => slot,
        _ => {
            if request.kind == Kind::Prompt {
                times.slot_wait = slot_started.elapsed();
                times.total = accepted.elapsed();
                let connected = peer_connected(&stream);
                let id = request.input.prompt_id.as_deref();
                prompt_stages::log_if_slow(Outcome::SlotTimeout, &times, true, connected, id);
            }
            return;
        }
    };
    times.slot_wait = slot_started.elapsed();
    if request.kind == Kind::PostCompact {
        let input = request.input;
        let _ = tokio::time::timeout_at(deadline, async {
            // Stored on the daemon's own connection: a write that may wait for
            // another writer, never on the render lane.
            let _slot = slot;
            let stored = tokio::task::spawn_blocking(move || {
                let _operation = engine.storage().operation_lock().ok()?;
                receipts
                    .with_connection(|conn| super::super::post_compact::store(conn, &input))
                    .ok()
            })
            .await
            .ok()
            .flatten()?;
            let bytes = serde_json::to_vec(&Response {
                version: VERSION,
                injection: if stored { "stored" } else { "duplicate" }.into(),
                reason: None,
            })
            .ok()?;
            write_frame(&mut stream, &bytes).await
        })
        .await;
        return;
    }
    let Some(cwd) = request.input.cwd.as_deref().map(PathBuf::from) else {
        return;
    };
    if !cwd.is_absolute() {
        return;
    }
    let runtime = tokio::runtime::Handle::current();
    // Not bounded by the deadline: a render runs to its end (it cannot be
    // cancelled), and its line is written there, after the client may be gone.
    let _ = tokio::task::spawn_blocking(move || {
        // Someone is waiting on this answer: run above the daemon's utility
        // threads and the user's default-class processes, then hand the
        // blocking-pool thread back at its own class.
        let _qos = crate::runtime::raise_thread_qos(crate::runtime::Qos::UserInitiated);
        #[cfg(test)]
        record_render_qos(request.input.session_id.as_deref());
        let input = &request.input;
        let rendered = Rendering {
            engine: &engine,
            receipts,
            input,
            cwd: &cwd,
            read_only,
            accepted,
            deadline,
            runtime: &runtime,
        };
        let (outcome, connected) = rendered.render_and_reply(&mut stream, slot, &mut times);
        let after_deadline = times.total >= times.deadline;
        let id = input.prompt_id.as_deref();
        prompt_stages::log_if_slow(outcome, &times, after_deadline, connected, id);
    })
    .await;
}

/// What one render needs, on its blocking worker.
#[cfg(unix)]
struct Rendering<'a> {
    engine: &'a Arc<Engine>,
    receipts: Arc<crate::storage::Storage>,
    input: &'a HookInput,
    cwd: &'a Path,
    read_only: bool,
    accepted: tokio::time::Instant,
    deadline: tokio::time::Instant,
    runtime: &'a tokio::runtime::Handle,
}

/// The answer is over: stamp its total and say whether the client is still
/// connected. Called before the receipts are written or an unshown claim is
/// released, since either may wait for the database writer.
#[cfg(unix)]
fn ended(
    accepted: tokio::time::Instant,
    times: &mut AnswerTimes,
    stream: &tokio::net::UnixStream,
) -> bool {
    times.total = accepted.elapsed();
    peer_connected(stream)
}

#[cfg(unix)]
impl Rendering<'_> {
    /// Render, then write the reply and wait for the acknowledgement, all
    /// before the deadline. The worker owns admission until rendering and
    /// delivery have finished. It lets go before anything that can wait for a
    /// database writer: the retrieval events and the release of an unshown
    /// dream claim, which go to the daemon's connection, not the render's.
    /// Returns the outcome and whether the client was still connected.
    fn render_and_reply(
        self,
        stream: &mut tokio::net::UnixStream,
        slot: tokio::sync::OwnedSemaphorePermit,
        times: &mut AnswerTimes,
    ) -> (Outcome, bool) {
        let accepted = self.accepted;
        let mut permit = Some(slot);
        let Ok(_operation) = self.engine.storage().operation_lock() else {
            return (Outcome::Busy, ended(accepted, times, stream));
        };
        #[cfg(test)]
        if let Some(delay) = test_render_delay(self.input) {
            std::thread::sleep(delay);
        }
        if tokio::time::Instant::now() >= self.deadline {
            return (Outcome::Late, ended(accepted, times, stream));
        }
        let render_started = Instant::now();
        let (prepared, steps) = self.runtime.block_on(prompt_stages::measure(
            super::super::prompt_submit::prepare_with_receipts_mode(
                self.input,
                self.engine,
                self.cwd,
                self.receipts,
                self.read_only,
            ),
        ));
        times.render = render_started.elapsed();
        times.steps = steps;
        let Ok(prepared) = prepared else {
            return (Outcome::Failed, ended(accepted, times, stream));
        };
        if tokio::time::Instant::now() >= self.deadline {
            let connected = ended(accepted, times, stream);
            drop(permit.take());
            drop(prepared);
            return (Outcome::Late, connected);
        }
        let Ok(bytes) = serde_json::to_vec(&Response {
            version: VERSION,
            injection: prepared.text.clone(),
            reason: prepared.reason,
        }) else {
            let connected = ended(accepted, times, stream);
            drop(permit.take());
            drop(prepared);
            return (Outcome::Failed, connected);
        };
        let reply_started = Instant::now();
        let delivered = self.runtime.block_on(async {
            tokio::time::timeout_at(self.deadline, async {
                write_frame(stream, &bytes).await?;
                (stream.read_u8().await.ok()? == 1).then_some(())
            })
            .await
            .ok()
            .flatten()
        });
        times.reply = reply_started.elapsed();
        if delivered.is_some() {
            times.total = accepted.elapsed();
            prepared.delivered_releasing(|| drop(permit.take()));
            (Outcome::Delivered, true)
        } else {
            let connected = ended(accepted, times, stream);
            // An unacknowledged plan drops here, on this blocking worker:
            // speculative dream claims are released and unshown exposures are
            // discarded, never on a Tokio worker.
            drop(permit.take());
            drop(prepared);
            (Outcome::Undelivered, connected)
        }
    }
}

/// Whether the client still holds its end of the stream. It sends nothing
/// after its request until it acknowledges a reply, so a peek finds either no
/// data (connected) or the end of the stream (gone).
#[cfg(unix)]
fn peer_connected(stream: &tokio::net::UnixStream) -> bool {
    use std::os::fd::AsRawFd;
    let mut byte = 0u8;
    // SAFETY: a one-byte peek into a live local buffer on the stream's own
    // open descriptor; MSG_DONTWAIT keeps it from blocking, MSG_PEEK from
    // consuming anything.
    let read = unsafe {
        libc::recv(
            stream.as_raw_fd(),
            (&mut byte as *mut u8).cast(),
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    match read {
        0 => false,
        n if n > 0 => true,
        _ => std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock,
    }
}

/// The class each test render ran at, by session id.
#[cfg(test)]
pub(super) static RENDER_QOS: std::sync::Mutex<Vec<(String, crate::runtime::Qos)>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(test)]
fn record_render_qos(session: Option<&str>) {
    if let (Some(session), Ok(mut seen)) = (session, RENDER_QOS.lock()) {
        seen.push((session.to_owned(), crate::runtime::thread_qos()));
    }
}

/// Test stand-in for a slow render: a session id of `render-delay-ms=<n>`
/// holds the render slot for `n` ms before it renders.
#[cfg(test)]
fn test_render_delay(input: &HookInput) -> Option<Duration> {
    let rest = input
        .session_id
        .as_deref()?
        .strip_prefix("render-delay-ms=")?;
    Some(Duration::from_millis(rest.parse().ok()?))
}
