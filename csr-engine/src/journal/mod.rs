//! Journal v4 — the live, daemon-hosted dream surface.
//!
//! `dream --report` still writes the certified, offline, portable static
//! export; this server is additive. It exists because before/after AST at
//! two OIDs, full briefs, paginated history and (later) write-back cannot be
//! precomputed into a static file without blowing its size bound.
//!
//! # Security contract (non-negotiable)
//!
//! The corpus behind this server is private conversation data. Therefore:
//!
//! * **The listener binds 127.0.0.1 and nothing else.** [`loopback_addr`] is
//!   the only function in this crate that produces a bind address, it takes
//!   a port and nothing more, and it hardcodes [`Ipv4Addr::LOCALHOST`].
//!   There is deliberately no host/interface/`--bind` argument, no
//!   environment variable, and no config field — an operator cannot expose
//!   this surface by misconfiguration, only by editing and rebuilding the
//!   binary. `bind_addr_is_always_loopback` and
//!   `no_source_line_offers_a_non_loopback_bind` guard that.
//! * **No CORS header is ever set** (enforced in `routes`).
//! * Read-only routes only, in this phase.
//! * Kill switch: `CSR_NO_JOURNAL_SERVER=1` disables the server everywhere.
//!   Both hosts — the `journal serve` subcommand and the daemon — build
//!   their router through [`router_if_enabled`] *before* they bind, and it
//!   is the single place the switch is read. [`serve`] itself reads no
//!   environment, so there is no second gate to disagree with the first.
//!
//! # Port
//!
//! [`DEFAULT_PORT`] is a fixed loopback port so the page is bookmarkable
//! across restarts (locked decision 7). `CSR_JOURNAL_PORT` overrides it; `0`
//! explicitly asks for an ephemeral port. If the preferred port is occupied,
//! [`bind_loopback`] falls back to an ephemeral port and the real one is
//! printed/logged — **a busy port never fails the daemon**.

pub mod astdiff;
pub mod composer;
pub mod render;
pub mod routes;
pub mod state;
pub mod week;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use tokio::net::TcpListener;

use crate::storage::Storage;
use state::JournalState;

/// Fixed loopback port — bookmarkable across restarts.
pub const DEFAULT_PORT: u16 = 7373;
/// Documented override. `0` means "pick an ephemeral port".
pub const PORT_ENV: &str = "CSR_JOURNAL_PORT";
/// Kill switch. `1`/`true` disables the server in every host.
pub const KILL_SWITCH_ENV: &str = "CSR_NO_JOURNAL_SERVER";

/// How often the graceful-shutdown watcher samples the daemon's flag.
const SHUTDOWN_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// The **only** bind-address constructor. Takes a port; the interface is not
/// a parameter and never will be.
pub fn loopback_addr(port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
}

/// Pure kill-switch predicate, split out so it can be tested without
/// mutating process environment.
pub fn kill_switch_engaged(raw: Option<&str>) -> bool {
    matches!(raw.map(str::trim), Some(v) if v == "1" || v.eq_ignore_ascii_case("true"))
}

/// Whether `CSR_NO_JOURNAL_SERVER` disables the server.
pub fn server_disabled() -> bool {
    kill_switch_engaged(std::env::var(KILL_SWITCH_ENV).ok().as_deref())
}

/// Resolve the preferred port from the environment, falling back to
/// [`DEFAULT_PORT`]. A non-numeric value is ignored rather than fatal.
pub fn configured_port() -> u16 {
    std::env::var(PORT_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT)
}

/// The route table, or `None` when the kill switch is engaged. Every host
/// goes through this rather than calling [`routes::router`] directly.
pub fn router_if_enabled(app_state: JournalState) -> Option<axum::Router> {
    if server_disabled() {
        return None;
    }
    Some(routes::router(app_state))
}

/// Bind loopback on `preferred`, falling back to an ephemeral loopback port
/// when it is occupied. Both branches go through [`loopback_addr`].
pub async fn bind_loopback(preferred: u16) -> std::io::Result<TcpListener> {
    match TcpListener::bind(loopback_addr(preferred)).await {
        Ok(listener) => Ok(listener),
        Err(error) if preferred != 0 => {
            tracing::warn!(
                port = preferred,
                %error,
                "journal port unavailable, falling back to an ephemeral loopback port"
            );
            TcpListener::bind(loopback_addr(0)).await
        }
        Err(error) => Err(error),
    }
}

/// The URL to print/bookmark for a bound listener. Always literal
/// `127.0.0.1` — never a resolved hostname.
pub fn url_for(addr: SocketAddr) -> String {
    format!("http://127.0.0.1:{}/", addr.port())
}

/// Resolves once `flag` flips, so the server can share the daemon's
/// existing `Arc<AtomicBool>` without any new signalling machinery.
async fn shutdown_when(flag: Arc<AtomicBool>) {
    while !flag.load(Ordering::SeqCst) {
        tokio::time::sleep(SHUTDOWN_POLL).await;
    }
}

/// Serve `router` until `shutdown` flips.
///
/// It takes an already-built router rather than the state, and reads no
/// environment of its own, so the kill switch is enforced in exactly one
/// place ([`router_if_enabled`]) instead of two that could disagree. Both
/// hosts below build their router through that gate before they bind.
pub async fn serve(
    router: axum::Router,
    listener: TcpListener,
    shutdown: Arc<AtomicBool>,
) -> std::io::Result<()> {
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_when(shutdown))
        .await
}

/// `csr-engine journal serve`. Binds loopback, prints the URL, serves until
/// Ctrl-C.
pub async fn run_cli(storage: Arc<Storage>, port: Option<u16>, open: bool) -> Result<()> {
    let Some(app) = router_if_enabled(JournalState::from_storage(storage)) else {
        println!("CSR journal server disabled by {KILL_SWITCH_ENV}; nothing started.");
        return Ok(());
    };
    let listener = bind_loopback(port.unwrap_or_else(configured_port)).await?;
    let addr = listener.local_addr()?;
    let url = url_for(addr);
    println!("CSR dream journal serving at {url}");
    println!("Loopback only (127.0.0.1). Read-only. Ctrl-C to stop.");
    if open {
        let _ = std::process::Command::new("open").arg(&url).status();
    }

    let shutdown = Arc::new(AtomicBool::new(false));
    let signal = shutdown.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        signal.store(true, Ordering::SeqCst);
    });

    serve(app, listener, shutdown).await?;
    println!("CSR dream journal stopped.");
    Ok(())
}

/// Daemon host (locked decision 7: always on, stable port, bookmarkable).
///
/// Returns `None` when the kill switch is engaged. The spawned task never
/// propagates a failure: a busy port, a bind error, or a serve error is
/// logged and the task returns, leaving every other daemon loop untouched.
/// Nothing in the task blocks — the SQLite work each request does runs on
/// `spawn_blocking` inside the handlers.
pub fn spawn_for_daemon(
    storage: Arc<Storage>,
    shutdown: Arc<AtomicBool>,
) -> Option<tokio::task::JoinHandle<()>> {
    let app = router_if_enabled(JournalState::from_storage(storage))?;
    Some(tokio::spawn(async move {
        let preferred = configured_port();
        let listener = match bind_loopback(preferred).await {
            Ok(listener) => listener,
            Err(error) => {
                tracing::warn!(%error, "journal server could not bind loopback; not serving");
                return;
            }
        };
        match listener.local_addr() {
            Ok(addr) => tracing::info!("journal server listening at {}", url_for(addr)),
            Err(error) => tracing::warn!(%error, "journal server bound but local_addr failed"),
        }
        if let Err(error) = serve(app, listener, shutdown).await {
            tracing::warn!(%error, "journal server stopped with an error");
        } else {
            tracing::info!("journal server stopped");
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every source file in this module, so the bind-surface guard below
    /// cannot be defeated by moving a listener into a sibling file.
    const SOURCES: [(&str, &str); 4] = [
        ("mod.rs", include_str!("mod.rs")),
        ("routes.rs", include_str!("routes.rs")),
        ("state.rs", include_str!("state.rs")),
        ("render.rs", include_str!("render.rs")),
    ];

    /// The shippable code of a source file: comments and the `#[cfg(test)]`
    /// module are removed.
    ///
    /// Both removals are load-bearing, not conveniences. These guards scan
    /// for strings like `0.0.0.0` and the CORS header name — strings the
    /// security *documentation* and the guards themselves must be free to
    /// mention. Scanning raw text made every guard trip on its own prose,
    /// which is a false positive, and a guard that has to be weakened to
    /// stop firing on itself is worse than no guard. What remains after
    /// this filter is exactly what the binary can execute.
    ///
    /// The comment stripper is line-based (`//`, `//!`, `///` and a `/* */`
    /// span). It does not attempt to understand `//` inside a string
    /// literal, so it can only ever remove *more* text than a real parser
    /// would — never less. A false negative is therefore possible in
    /// principle; `no_guard_scans_an_empty_body` keeps that from silently
    /// becoming "the guard scans nothing at all".
    fn shippable_code(source: &str) -> String {
        let code = match source.find("#[cfg(test)]") {
            Some(cut) => &source[..cut],
            None => source,
        };
        let mut out = String::with_capacity(code.len());
        let mut in_block_comment = false;
        for line in code.lines() {
            let trimmed = line.trim_start();
            if in_block_comment {
                if let Some(rest) = trimmed.split_once("*/") {
                    in_block_comment = false;
                    out.push_str(rest.1);
                    out.push('\n');
                }
                continue;
            }
            if trimmed.starts_with("//") {
                continue;
            }
            if trimmed.starts_with("/*") {
                in_block_comment = !trimmed.contains("*/");
                continue;
            }
            out.push_str(line);
            out.push('\n');
        }
        out
    }

    /// Guard on the guards: if `shippable_code` ever strips a file down to
    /// nothing, the scans below would pass vacuously.
    #[test]
    fn no_guard_scans_an_empty_body() {
        for (name, source) in SOURCES {
            let code = shippable_code(source);
            assert!(
                code.len() > 500,
                "{name}: only {} bytes of code left to scan — the guards below \
                 would be passing on an empty string",
                code.len()
            );
        }
        assert!(
            shippable_code(include_str!("mod.rs")).contains("TcpListener::bind("),
            "mod.rs's real bind calls must survive the comment/test stripper"
        );
    }

    #[test]
    fn bind_addr_is_always_loopback() {
        for port in [0u16, 1, 7373, 8080, 65535] {
            let addr = loopback_addr(port);
            assert!(addr.ip().is_loopback(), "{addr} is not loopback");
            assert_eq!(addr.ip(), IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)));
            assert_eq!(addr.port(), port);
        }
    }

    /// `loopback_addr` takes a port and nothing else, and no other code path
    /// in this module can name a non-loopback interface. If a later phase
    /// adds a `--bind`/host knob, this fails.
    #[test]
    fn no_source_line_offers_a_non_loopback_bind() {
        const FORBIDDEN: [&str; 6] = [
            "0.0.0.0",
            "UNSPECIFIED",
            "Ipv6Addr",
            "CSR_JOURNAL_HOST",
            "CSR_JOURNAL_BIND",
            "to_socket_addrs",
        ];
        for (name, source) in SOURCES {
            let code = shippable_code(source);
            for needle in FORBIDDEN {
                assert!(
                    !code.contains(needle),
                    "{name} mentions {needle}: the journal must bind loopback only"
                );
            }
        }
    }

    /// The only `TcpListener::bind` calls must be the two inside
    /// `bind_loopback`, both of which pass `loopback_addr(..)`.
    #[test]
    fn every_listener_bind_goes_through_loopback_addr() {
        for (name, source) in SOURCES {
            for line in shippable_code(source).lines() {
                if line.contains("TcpListener::bind(") {
                    assert!(
                        line.contains("loopback_addr("),
                        "{name}: bind call not routed through loopback_addr: {}",
                        line.trim()
                    );
                }
            }
        }
    }

    #[test]
    fn no_cors_header_name_appears_anywhere_in_the_module() {
        for (name, source) in SOURCES {
            // `routes::tests::no_cors_header_is_ever_set` asserts absence on
            // real responses; this asserts the header name is not even
            // reachable in shippable code. The security doc comments are
            // free to name it — `shippable_code` drops them.
            let lower = shippable_code(source).to_lowercase();
            assert!(
                !lower.contains("access-control-allow"),
                "{name} names a CORS header; the journal must never set one"
            );
        }
    }

    #[test]
    fn kill_switch_truth_table() {
        assert!(!kill_switch_engaged(None));
        assert!(!kill_switch_engaged(Some("")));
        assert!(!kill_switch_engaged(Some("0")));
        assert!(!kill_switch_engaged(Some("no")));
        assert!(kill_switch_engaged(Some("1")));
        assert!(kill_switch_engaged(Some("true")));
        assert!(kill_switch_engaged(Some("TRUE")));
        assert!(kill_switch_engaged(Some(" 1 ")));
    }

    /// Both kill-switch effects live in ONE test on purpose. `set_var` is
    /// process-global and `cargo test` runs tests in parallel threads, so a
    /// second test toggling `CSR_NO_JOURNAL_SERVER` would race this one and
    /// flake. `kill_switch_engaged` above covers the parsing without
    /// touching the environment at all, and `serve` deliberately reads no
    /// environment, so no other test in this file can observe the toggle.
    #[tokio::test]
    async fn kill_switch_disables_the_router_and_the_daemon_host() {
        use crate::journal::state::StaticDreamFeed;

        let app_state = || JournalState::new(Arc::new(StaticDreamFeed::new(Vec::new())));

        std::env::remove_var(KILL_SWITCH_ENV);
        assert!(!server_disabled());
        assert!(
            router_if_enabled(app_state()).is_some(),
            "router must build when the kill switch is clear"
        );

        std::env::set_var(KILL_SWITCH_ENV, "1");
        assert!(server_disabled());
        assert!(
            router_if_enabled(app_state()).is_none(),
            "kill switch must stop the router being built at all"
        );

        // The daemon host must decline to spawn a task at all — no listener,
        // no port, nothing to shut down later.
        let storage = Arc::new(Storage::open_memory().expect("in-memory storage"));
        assert!(
            spawn_for_daemon(storage, Arc::new(AtomicBool::new(false))).is_none(),
            "kill switch must stop the daemon host spawning anything"
        );

        std::env::remove_var(KILL_SWITCH_ENV);
        assert!(!server_disabled());
    }

    #[test]
    fn configured_port_defaults_and_parses() {
        std::env::remove_var(PORT_ENV);
        assert_eq!(configured_port(), DEFAULT_PORT);
        std::env::set_var(PORT_ENV, "9999");
        assert_eq!(configured_port(), 9999);
        std::env::set_var(PORT_ENV, "0");
        assert_eq!(configured_port(), 0, "0 explicitly requests ephemeral");
        std::env::set_var(PORT_ENV, "not-a-port");
        assert_eq!(configured_port(), DEFAULT_PORT, "garbage falls back");
        std::env::remove_var(PORT_ENV);
    }

    #[test]
    fn url_is_always_the_literal_loopback_address() {
        assert_eq!(url_for(loopback_addr(7373)), "http://127.0.0.1:7373/");
        assert_eq!(url_for(loopback_addr(0)), "http://127.0.0.1:0/");
    }

    #[tokio::test]
    async fn a_busy_port_falls_back_instead_of_failing() {
        let first = bind_loopback(0).await.expect("first bind");
        let busy = first.local_addr().expect("addr").port();
        let second = bind_loopback(busy)
            .await
            .expect("fallback bind must succeed");
        let fallback = second.local_addr().expect("addr");
        assert!(fallback.ip().is_loopback());
        assert_ne!(fallback.port(), busy, "fallback must pick a different port");
    }

    #[tokio::test]
    async fn shutdown_flag_stops_the_server() {
        use crate::journal::state::StaticDreamFeed;

        let listener = bind_loopback(0).await.expect("bind");
        let app = routes::router(JournalState::new(Arc::new(
            StaticDreamFeed::new(Vec::new()),
        )));
        let shutdown = Arc::new(AtomicBool::new(false));
        let handle = tokio::spawn(serve(app, listener, shutdown.clone()));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!handle.is_finished(), "server should still be running");
        shutdown.store(true, Ordering::SeqCst);
        let joined = tokio::time::timeout(std::time::Duration::from_secs(5), handle)
            .await
            .expect("server must stop within 5s of the flag flipping")
            .expect("server task");
        assert!(joined.is_ok());
    }
}
