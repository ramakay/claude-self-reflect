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
