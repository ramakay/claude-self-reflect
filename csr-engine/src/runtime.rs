//! Tokio runtime sizing. Extracted out of `main.rs` (which is not exercised
//! by `cargo test --lib`) so the env-parsing logic is unit-testable.

/// Soak-rig clock. `CSR_TIME_SCALE=N` (N > 1, default unset = 1) makes the daemon's
/// whole-tick loops (the `for _ in 0..360 { sleep(10 s) }` family: hourly maintenance,
/// 10- and 30-minute import sweeps, compaction, memory health, ...) wait `10 s / N` per
/// tick, floored at 20 ms so a very large N cannot spin a loop. Timers that already have
/// their own env override (`CSR_GEN_REGEN_MIN_SECS`, `CSR_DREAM_INTERVAL_SECS`, ...) are
/// untouched; wall-clock TTLs stored in the database stay real time.
pub const TIME_SCALE_ENV: &str = "CSR_TIME_SCALE";
const MIN_TICK: std::time::Duration = std::time::Duration::from_millis(20);

/// The parsed scale: finite and above 1, else 1.0 (no scaling).
pub fn time_scale() -> f64 {
    static SCALE: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *SCALE.get_or_init(|| parse_time_scale(std::env::var(TIME_SCALE_ENV).ok().as_deref()))
}

pub(crate) fn parse_time_scale(raw: Option<&str>) -> f64 {
    raw.and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|n| n.is_finite() && *n > 1.0)
        .unwrap_or(1.0)
}

/// One loop tick of `secs` seconds under `scale`.
pub(crate) fn scaled_tick(secs: u64, scale: f64) -> std::time::Duration {
    let full = std::time::Duration::from_secs(secs);
    if scale <= 1.0 {
        return full;
    }
    full.div_f64(scale).max(MIN_TICK.min(full))
}

/// A loop tick of `secs` seconds on the soak clock; exactly `secs` when `CSR_TIME_SCALE` is unset.
pub fn loop_tick(secs: u64) -> std::time::Duration {
    scaled_tick(secs, time_scale())
}

/// `secs` of wall time on the soak clock, at least 1 s; exactly `secs` when unset.
pub fn scaled_secs(secs: i64) -> i64 {
    let scale = time_scale();
    if scale <= 1.0 {
        return secs;
    }
    ((secs as f64 / scale).round() as i64).max(1)
}

/// Largest worker count an override may request; anything above falls back
/// to the default like junk input does, so a typo cannot spawn thousands of
/// OS threads.
pub const MAX_THREAD_OVERRIDE: usize = 64;

/// Resolve the tokio multi-thread runtime's worker-thread count:
/// `CSR_TOKIO_WORKERS` if it parses as `1..=MAX_THREAD_OVERRIDE`, else
/// `min(4, available_parallelism)`. An unbounded `#[tokio::main]` runtime
/// spins up one worker per core, which is wasted on a process that is
/// mostly a stdio-driven MCP server or a one-shot hook — cap it the same
/// way `CSR_EMBED_THREADS` caps ORT.
pub fn resolve_tokio_workers() -> usize {
    if let Ok(raw) = std::env::var("CSR_TOKIO_WORKERS") {
        if let Ok(n) = raw.trim().parse::<usize>() {
            if (1..=MAX_THREAD_OVERRIDE).contains(&n) {
                return n;
            }
        }
    }
    std::thread::available_parallelism()
        .map(|n| n.get().min(4))
        .unwrap_or(4)
}

/// A thread's macOS quality-of-service class (`qos_class_t`), which sets its
/// scheduling priority: user-initiated 37, default 31, utility 20. A new
/// pthread starts at default whatever its creator's class. Under launchd
/// `ProcessType Interactive` the class decides; under `Standard` every thread
/// is held at 20 (measured with `Nice 10` on the Studio, 2026-11-16).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Qos {
    UserInteractive,
    UserInitiated,
    Default,
    Utility,
    Background,
    Unspecified,
}

impl Qos {
    #[cfg(target_os = "macos")]
    fn raw(self) -> u32 {
        match self {
            Qos::UserInteractive => 0x21,
            Qos::UserInitiated => 0x19,
            Qos::Default => 0x15,
            Qos::Utility => 0x11,
            Qos::Background => 0x09,
            Qos::Unspecified => 0,
        }
    }

    #[cfg(target_os = "macos")]
    fn from_raw(raw: u32) -> Self {
        match raw {
            0x21 => Qos::UserInteractive,
            0x19 => Qos::UserInitiated,
            0x15 => Qos::Default,
            0x11 => Qos::Utility,
            0x09 => Qos::Background,
            _ => Qos::Unspecified,
        }
    }
}

#[cfg(target_os = "macos")]
extern "C" {
    fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
    fn pthread_get_qos_class_np(
        thread: libc::pthread_t,
        qos_class: *mut u32,
        relative_priority: *mut i32,
    ) -> i32;
}

/// Set the calling thread's class. `false` where there is none (not macOS).
pub fn set_thread_qos(qos: Qos) -> bool {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: plain libpthread call on the current thread.
        unsafe { pthread_set_qos_class_self_np(qos.raw(), 0) == 0 }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = qos;
        false
    }
}

/// The calling thread's class; `Unspecified` where there is none.
pub fn thread_qos() -> Qos {
    #[cfg(target_os = "macos")]
    {
        let (mut raw, mut relative) = (0u32, 0i32);
        // SAFETY: out-pointers to locals, for the current thread.
        let this = unsafe { libc::pthread_self() };
        if unsafe { pthread_get_qos_class_np(this, &mut raw, &mut relative) } == 0 {
            return Qos::from_raw(raw);
        }
    }
    Qos::Unspecified
}

/// Puts the calling thread back in its earlier class when dropped.
pub struct QosScope(Qos);

/// Run the calling thread at `qos` until the scope drops. For a prompt render
/// on a shared blocking-pool thread, which then goes back to background work.
pub fn raise_thread_qos(qos: Qos) -> QosScope {
    let previous = thread_qos();
    set_thread_qos(qos);
    QosScope(previous)
}

impl Drop for QosScope {
    fn drop(&mut self) {
        if self.0 != Qos::Unspecified {
            set_thread_qos(self.0);
        }
    }
}

/// The process runtime. For the daemon (`background`) every worker and
/// blocking thread, and the calling thread that drives the daemon's future,
/// runs at utility: what launchd's `Standard` clamp gave them, so background
/// work does not gain priority when the agent runs `Interactive`. The prompt
/// render raises its own thread for the render.
pub fn build_main_runtime(
    workers: usize,
    background: bool,
) -> std::io::Result<tokio::runtime::Runtime> {
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    builder.worker_threads(workers).enable_all();
    if background {
        set_thread_qos(Qos::Utility);
        builder.on_thread_start(|| {
            set_thread_qos(Qos::Utility);
        });
    }
    builder.build()
}

/// Raise this process's open-file soft limit toward its hard limit at startup.
///
/// With mmap on, loading an index has `hnsw_rs` open the graph and data files
/// several times over (its `init()` holds both, then `from_hnswdump` reopens the
/// graph). `hnsw_rs` 0.3.4 responds to an open/map failure with
/// `std::process::exit(1)`, so a low descriptor ceiling — macOS defaults the soft
/// `RLIMIT_NOFILE` to 256 — could turn transient `EMFILE` during load into a hard
/// exit instead of the heap-rebuild fallback. Lifting the soft limit to the hard
/// limit (capped, so macOS's `OPEN_MAX` is respected) removes that trigger. Needs
/// no privilege: raising the soft limit up to the hard limit is always allowed.
#[cfg(unix)]
pub fn raise_fd_limit() {
    // Cap the target so a huge/INFINITY hard limit does not exceed a kernel maximum
    // (macOS rejects rlim_cur above OPEN_MAX).
    const TARGET: u64 = 10_240;
    // SAFETY: getrlimit/setrlimit with a valid resource id and a stack-allocated
    // rlimit struct; we only ever raise the soft limit toward the hard limit.
    unsafe {
        let mut lim = std::mem::MaybeUninit::<libc::rlimit>::zeroed().assume_init();
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) != 0 {
            return;
        }
        let desired = std::cmp::min(lim.rlim_max, TARGET as libc::rlim_t);
        if lim.rlim_cur < desired {
            lim.rlim_cur = desired;
            let _ = libc::setrlimit(libc::RLIMIT_NOFILE, &lim);
        }
    }
}

/// No-op on non-unix (no `RLIMIT_NOFILE`; mmap is disabled there anyway).
#[cfg(not(unix))]
pub fn raise_fd_limit() {}

/// Ask the allocator to return freed-but-retained pages to the OS.
///
/// After the HNSW index is built or reconciled, the process has a large amount
/// of transient allocation (batch buffers, the model's tokenizer scratch) that
/// is freed but that macOS's `malloc` keeps as dirty pages in the process — the
/// ~130 MB of allocator slack that makes a long-lived server's `vmmap` physical
/// footprint bimodal on the same binary and DB. `heap -s` live bytes are
/// deterministic; the swing is entirely these retained pages. Returning them
/// makes the footprint an honest, low-variance reflection of live memory, which
/// is what Activity Monitor shows.
///
/// macOS-only via `malloc_zone_pressure_relief(NULL, 0)` (releases across every
/// zone; the documented way to hint the allocator under memory pressure). A
/// no-op elsewhere — glibc reclaims on its own and there is no portable hook.
#[cfg(target_os = "macos")]
pub fn release_freed_pages() {
    // SAFETY: `malloc_zone_pressure_relief` is part of the system malloc API in
    // libSystem. A null zone pointer means "all zones"; a goal of 0 means
    // "release as much as possible". It takes no ownership and returns the byte
    // count released, which we ignore.
    unsafe {
        extern "C" {
            fn malloc_zone_pressure_relief(zone: *mut core::ffi::c_void, goal: usize) -> usize;
        }
        let _ = malloc_zone_pressure_relief(core::ptr::null_mut(), 0);
    }
}

/// No-op on non-macOS targets (see the macOS variant's docs).
#[cfg(not(target_os = "macos"))]
pub fn release_freed_pages() {}

/// Drive `fut` to completion on `runtime`, then shut the runtime down with a
/// bounded wait, and return `fut`'s output. This is `main`'s entire runtime
/// lifecycle — extracted so it is unit-testable (`main` itself is not
/// exercised by `cargo test --lib`) and so the shutdown-bounding fix has a
/// test that actually calls the function `main` calls, not just a
/// `tokio::runtime::Runtime` a test built and drove itself.
///
/// WP-D high finding: dropping a `tokio::runtime::Runtime` blocks the
/// dropping thread until every task on its blocking pool returns (tokio
/// 1.53.1 `src/runtime/runtime.rs`, `src/task/blocking.rs`). `fut` (`main`'s
/// `run()`) can return while the daemon's compaction loop has abandoned
/// *awaiting* (not cancelled) a `spawn_blocking` streaming HNSW rebuild — a
/// bare `runtime.block_on(fut)` followed by an implicit drop would then hang
/// for however long that rebuild takes (minutes on the real corpus, well
/// past whatever asked this process to stop — SIGTERM, an installer
/// upgrade). `shutdown_timeout` bounds that wait instead: the abandoned
/// closure only builds an in-memory `SearchEngine` nothing will read once
/// this process exits, so leaving its thread running past the timeout —
/// until the OS reclaims it on process exit — is safe.
pub fn run_to_completion<F: std::future::Future>(
    runtime: tokio::runtime::Runtime,
    fut: F,
    timeout: std::time::Duration,
) -> F::Output {
    let result = runtime.block_on(fut);
    runtime.shutdown_timeout(timeout);
    result
}

/// Run an async job whose body is mostly synchronous (file parsing, SQLite,
/// CPU-bound training) on the blocking pool, driven there by the current
/// runtime, instead of on one of the daemon's few async workers. A worker
/// stuck in such a job stalls every task scheduled behind it — the prompt
/// socket's accept loop included. A panic in the job comes back as an error.
pub(crate) async fn run_off_worker<F, Fut, T>(job: F) -> anyhow::Result<T>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = T>,
    T: Send + 'static,
{
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || runtime.block_on(job()))
        .await
        .map_err(|e| anyhow::anyhow!("off-worker job failed: {e}"))
}

/// The lib test binary never runs `main`, so nothing raised its descriptor ceiling: it ran under
/// the shell's soft limit (256 on macOS). Tests run in parallel, each holding a SQLite database
/// (three files), runtimes and sockets, and the ceiling was hit by whichever test opened the
/// next file: an `EMFILE` that showed up as a `hnsw_rs` "could not open file" panic or a failed
/// runtime build in an unrelated test. Only the later tests were safe, once
/// `raise_fd_limit_lifts_the_soft_limit` had run. This lifts the limit before the first test, as
/// `main` does for the binary.
#[cfg(all(test, unix))]
mod test_fd_limit {
    extern "C" fn raise() {
        super::raise_fd_limit();
    }

    #[used]
    #[cfg_attr(target_vendor = "apple", link_section = "__DATA,__mod_init_func")]
    #[cfg_attr(not(target_vendor = "apple"), link_section = ".init_array")]
    static RAISE_AT_LOAD: extern "C" fn() = raise;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_env<F: FnOnce()>(value: Option<&str>, f: F) {
        let _guard = ENV_LOCK.lock().unwrap();
        let previous = std::env::var("CSR_TOKIO_WORKERS").ok();
        match value {
            Some(v) => std::env::set_var("CSR_TOKIO_WORKERS", v),
            None => std::env::remove_var("CSR_TOKIO_WORKERS"),
        }
        f();
        match previous {
            Some(v) => std::env::set_var("CSR_TOKIO_WORKERS", v),
            None => std::env::remove_var("CSR_TOKIO_WORKERS"),
        }
    }

    #[test]
    fn unset_falls_back_to_default() {
        with_env(None, || {
            let expected = std::thread::available_parallelism()
                .map(|n| n.get().min(4))
                .unwrap_or(4);
            assert_eq!(resolve_tokio_workers(), expected);
        });
    }

    #[test]
    fn valid_override_is_used() {
        with_env(Some("3"), || {
            assert_eq!(resolve_tokio_workers(), 3);
        });
    }

    #[test]
    fn junk_falls_back_to_default() {
        with_env(Some("nope"), || {
            let expected = std::thread::available_parallelism()
                .map(|n| n.get().min(4))
                .unwrap_or(4);
            assert_eq!(resolve_tokio_workers(), expected);
        });
    }

    #[test]
    fn excessive_falls_back_to_default() {
        with_env(Some("4294967296"), || {
            let expected = std::thread::available_parallelism()
                .map(|n| n.get().min(4))
                .unwrap_or(4);
            assert_eq!(resolve_tokio_workers(), expected);
        });
        with_env(Some(&(MAX_THREAD_OVERRIDE + 1).to_string()), || {
            let expected = std::thread::available_parallelism()
                .map(|n| n.get().min(4))
                .unwrap_or(4);
            assert_eq!(resolve_tokio_workers(), expected);
        });
        with_env(Some(&MAX_THREAD_OVERRIDE.to_string()), || {
            assert_eq!(resolve_tokio_workers(), MAX_THREAD_OVERRIDE);
        });
    }

    #[cfg(unix)]
    #[test]
    fn the_test_binary_starts_with_the_limit_already_raised() {
        // Run alone, nothing else has called `raise_fd_limit`: only the load-time hook can have.
        let mut lim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: reads this process's RLIMIT_NOFILE into a valid struct.
        assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) }, 0);
        assert!(lim.rlim_cur >= std::cmp::min(lim.rlim_max, 10_240 as libc::rlim_t));
    }

    #[cfg(unix)]
    #[test]
    fn raise_fd_limit_lifts_the_soft_limit() {
        // SAFETY: reads this process's RLIMIT_NOFILE.
        let soft_before = unsafe {
            let mut lim = std::mem::MaybeUninit::<libc::rlimit>::zeroed().assume_init();
            assert_eq!(libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim), 0);
            lim
        };
        raise_fd_limit();
        let soft_after = unsafe {
            let mut lim = std::mem::MaybeUninit::<libc::rlimit>::zeroed().assume_init();
            assert_eq!(libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim), 0);
            lim
        };
        // Never lowers the limit, and raises it toward the hard limit when there is room.
        assert!(soft_after.rlim_cur >= soft_before.rlim_cur);
        let target = std::cmp::min(soft_before.rlim_max, 10_240 as libc::rlim_t);
        assert!(soft_after.rlim_cur >= target);
    }

    #[test]
    fn release_freed_pages_is_callable() {
        // No observable return; the contract is only that it never panics or
        // aborts, on macOS (real call) or elsewhere (no-op).
        release_freed_pages();
        release_freed_pages();
    }

    #[test]
    fn zero_falls_back_to_default() {
        with_env(Some("0"), || {
            let expected = std::thread::available_parallelism()
                .map(|n| n.get().min(4))
                .unwrap_or(4);
            assert_eq!(resolve_tokio_workers(), expected);
        });
    }

    // WP-D high finding: dropping a `tokio::runtime::Runtime` blocks the
    // dropping thread until every task on its blocking pool returns (tokio
    // 1.53.1 `src/runtime/runtime.rs`, `src/task/blocking.rs`). `main`'s
    // `runtime.block_on(run())` can return while the daemon's compaction
    // loop has abandoned *awaiting* (not cancelled) a `spawn_blocking`
    // streaming HNSW rebuild — a bare `block_on` there would then hang in
    // `Runtime::drop` for however long that rebuild takes (minutes on the
    // real corpus). `main` instead does
    // `let r = runtime.block_on(run()); runtime.shutdown_timeout(..); r` —
    // this test proves `shutdown_timeout` actually bounds that wait rather
    // than blocking for the abandoned task's full remaining duration.
    #[test]
    fn shutdown_timeout_bounds_the_wait_for_an_abandoned_blocking_task() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        // Simulate the abandoned `rebuild_search_index_streaming` closure:
        // long-running blocking work with no cancellation point, exactly
        // like a real HNSW insert loop mid-rebuild.
        runtime.spawn_blocking(|| {
            std::thread::sleep(std::time::Duration::from_secs(5));
        });

        // The rest of `run()` completing promptly, as it does once shutdown
        // wins the daemon's `select!`.
        runtime.block_on(async {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        });

        let t0 = std::time::Instant::now();
        runtime.shutdown_timeout(std::time::Duration::from_millis(200));
        let elapsed = t0.elapsed();

        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "shutdown_timeout must bound the wait for the abandoned blocking \
             task instead of blocking for its full remaining duration \
             (~5s here, minutes on the real corpus); elapsed={elapsed:?}"
        );
    }

    // Reviewer-found gap (WP-D high, second pass): the test above only
    // exercises `tokio::runtime::Runtime::shutdown_timeout` on a runtime the
    // test built and drove itself — it never touches `main`, so deleting
    // `runtime.shutdown_timeout(...)` from `main.rs` (reverting to a plain
    // `block_on` + implicit drop, the original bug) does not fail it. This
    // test instead calls `run_to_completion` — the exact function `main`
    // calls — with a future that spawns a 5s blocking task and abandons it,
    // and asserts the call returns in well under that 5s. Reverting
    // `run_to_completion` to a plain `let r = runtime.block_on(fut); r`
    // (dropping `runtime` implicitly, no `shutdown_timeout` call) fails
    // this test: dropping the `Runtime` blocks until the abandoned
    // `spawn_blocking` closure finishes, so `elapsed` would be ~5s.
    #[test]
    fn run_to_completion_bounds_the_wait_for_an_abandoned_blocking_task() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        let t0 = std::time::Instant::now();
        let result = run_to_completion(
            runtime,
            async {
                // Simulate the daemon's abandoned compaction rebuild: a
                // long-running blocking closure with no cancellation point,
                // started but never awaited before the future completes.
                tokio::task::spawn_blocking(|| {
                    std::thread::sleep(std::time::Duration::from_secs(5));
                });
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                42
            },
            std::time::Duration::from_millis(200),
        );
        let elapsed = t0.elapsed();

        assert_eq!(
            result, 42,
            "run_to_completion must still return the future's output"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "run_to_completion must bound the shutdown wait for the \
             abandoned blocking task instead of blocking for its full \
             remaining duration (~5s here); elapsed={elapsed:?}"
        );
    }

    #[tokio::test]
    async fn an_off_worker_job_returns_its_value_and_a_panic_as_an_error() {
        assert_eq!(run_off_worker(|| async { 7 }).await.unwrap(), 7);
        let failed = run_off_worker(|| async { panic!("boom") }).await;
        assert!(failed
            .unwrap_err()
            .to_string()
            .contains("off-worker job failed"));
    }

    #[test]
    fn time_scale_parses_only_values_above_one() {
        assert_eq!(parse_time_scale(None), 1.0);
        assert_eq!(parse_time_scale(Some("")), 1.0);
        assert_eq!(parse_time_scale(Some("junk")), 1.0);
        assert_eq!(parse_time_scale(Some("0")), 1.0);
        assert_eq!(parse_time_scale(Some("-5")), 1.0);
        assert_eq!(parse_time_scale(Some("1")), 1.0);
        assert_eq!(parse_time_scale(Some("NaN")), 1.0);
        assert_eq!(parse_time_scale(Some("inf")), 1.0);
        assert_eq!(parse_time_scale(Some(" 2000 ")), 2000.0);
    }

    #[test]
    fn scaled_tick_is_unchanged_without_a_scale_and_floored_with_one() {
        let ten = std::time::Duration::from_secs(10);
        assert_eq!(scaled_tick(10, 1.0), ten);
        assert_eq!(scaled_tick(10, 0.5), ten);
        assert_eq!(scaled_tick(10, 10.0), std::time::Duration::from_secs(1));
        assert_eq!(
            scaled_tick(10, 2000.0),
            std::time::Duration::from_millis(20)
        );
        assert_eq!(scaled_tick(10, 1e12), std::time::Duration::from_millis(20));
        assert_eq!(scaled_tick(0, 2000.0), std::time::Duration::ZERO);
        // an unset scale never shortens the real tick
        assert_eq!(loop_tick(10), ten);
        assert_eq!(scaled_secs(3600), 3600);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_raised_thread_goes_back_to_its_class_when_the_scope_drops() {
        std::thread::spawn(|| {
            assert!(set_thread_qos(Qos::Utility));
            assert_eq!(thread_qos(), Qos::Utility);
            {
                let _render = raise_thread_qos(Qos::UserInitiated);
                assert_eq!(thread_qos(), Qos::UserInitiated);
            }
            assert_eq!(thread_qos(), Qos::Utility);
        })
        .join()
        .unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_daemon_runtime_runs_its_workers_and_blocking_threads_at_utility() {
        std::thread::spawn(|| {
            let runtime = build_main_runtime(2, true).unwrap();
            assert_eq!(thread_qos(), Qos::Utility, "the thread driving the daemon");
            let (worker, blocking) = runtime.block_on(async {
                let worker = tokio::spawn(async { thread_qos() }).await.unwrap();
                let blocking = tokio::task::spawn_blocking(thread_qos).await.unwrap();
                (worker, blocking)
            });
            assert_eq!((worker, blocking), (Qos::Utility, Qos::Utility));
            drop(runtime);
            // Any other command leaves its threads' class alone.
            let plain = build_main_runtime(2, false).unwrap();
            let blocking =
                plain.block_on(async { tokio::task::spawn_blocking(thread_qos).await.unwrap() });
            assert_ne!(blocking, Qos::Utility);
        })
        .join()
        .unwrap();
    }
}
