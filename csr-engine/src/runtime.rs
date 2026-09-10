//! Tokio runtime sizing. Extracted out of `main.rs` (which is not exercised
//! by `cargo test --lib`) so the env-parsing logic is unit-testable.

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
}
