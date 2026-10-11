//! MCP over Streamable HTTP: one process, one index, many sessions.
//!
//! The stdio transport gives every Claude Code session its own `csr-engine`
//! process, and every one of those processes loads the whole HNSW index
//! before the first prompt is answered. On a machine running many sessions
//! at once that is the same index paid for over and over, in both resident
//! memory and startup latency.
//!
//! This module serves the same [`ServerHandler`] the stdio path serves, over
//! MCP Streamable HTTP, so a single long-lived process answers every session.
//! The handler is cloned per session (it holds `Arc`s over storage, the
//! embedding engine and the search index), so sessions share one loaded
//! index rather than each building their own.
//!
//! The endpoint is unauthenticated, so it binds loopback addresses only.
//! rmcp's own `Host` header allowlist (loopback names by default) guards
//! against DNS rebinding.

use std::net::{SocketAddr, ToSocketAddrs};

use anyhow::{anyhow, Context, Result};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::ServerHandler;
use tokio::net::TcpListener;

/// Path the MCP endpoint is mounted at. A client registers
/// `http://<addr>/mcp`.
pub const ENDPOINT_PATH: &str = "/mcp";

/// Resolve and validate a `--serve-http` listen address.
///
/// Accepts anything `host:port`, including `127.0.0.1:7391`, `[::1]:7391`
/// and `localhost:7391`. Non-loopback addresses are refused: the endpoint is
/// unauthenticated, and rmcp's `Host` allowlist only answers requests
/// addressed to loopback anyway.
pub fn parse_listen_addr(raw: &str) -> Result<SocketAddr> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!(
            "--serve-http needs an address such as 127.0.0.1:7391"
        ));
    }

    let addr = trimmed
        .to_socket_addrs()
        .with_context(|| format!("not a valid host:port address: {trimmed}"))?
        .next()
        .ok_or_else(|| anyhow!("address resolved to no socket address: {trimmed}"))?;

    if !addr.ip().is_loopback() {
        return Err(anyhow!(
            "refusing to bind {addr}: the MCP endpoint is unauthenticated, so it binds \
             loopback addresses only"
        ));
    }

    Ok(addr)
}

/// Build the router that mounts the MCP endpoint at [`ENDPOINT_PATH`].
///
/// The factory clones `handler` per session. Clone has to be cheap and has to
/// share state. That is what makes this one index load instead of many.
pub fn router<S>(handler: S) -> axum::Router
where
    S: ServerHandler + Clone + Send + Sync + 'static,
{
    router_with_config(handler, StreamableHttpServerConfig::default())
}

fn router_with_config<S>(handler: S, config: StreamableHttpServerConfig) -> axum::Router
where
    S: ServerHandler + Clone + Send + Sync + 'static,
{
    let service: StreamableHttpService<S, LocalSessionManager> =
        StreamableHttpService::new(move || Ok(handler.clone()), Default::default(), config);
    axum::Router::new().nest_service(ENDPOINT_PATH, service)
}

/// Bind the listen socket. Separate from [`serve`] so a caller can report the
/// address it actually got (a port of `0` resolves to a real port here).
pub async fn bind(addr: SocketAddr) -> Result<TcpListener> {
    TcpListener::bind(addr)
        .await
        .with_context(|| format!("cannot bind {addr} for --serve-http"))
}

/// Serve `handler` over MCP Streamable HTTP until Ctrl+C.
pub async fn serve<S>(handler: S, listener: TcpListener) -> Result<()>
where
    S: ServerHandler + Clone + Send + Sync + 'static,
{
    serve_until(handler, listener, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
}

/// [`serve`] with the shutdown trigger passed in, so a test can stop the
/// server without signaling the whole test process.
///
/// When `shutdown` completes the listener stops accepting and every open
/// session is cancelled. The cancel matters: a connected client holds a GET
/// event stream open for the life of its session, and axum's graceful
/// shutdown waits for every connection to close, so without it shutdown
/// would wait on the clients instead of returning.
pub async fn serve_until<S, F>(handler: S, listener: TcpListener, shutdown: F) -> Result<()>
where
    S: ServerHandler + Clone + Send + Sync + 'static,
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let config = StreamableHttpServerConfig::default();
    let sessions = config.cancellation_token.clone();
    axum::serve(listener, router_with_config(handler, config))
        .with_graceful_shutdown(async move {
            shutdown.await;
            sessions.cancel();
        })
        .await
        .context("MCP Streamable HTTP server stopped")
}

/// The line printed once the endpoint is up, so an operator can copy the URL
/// straight into an MCP client config.
pub fn ready_line(addr: SocketAddr) -> String {
    format!("CSR: MCP Streamable HTTP ready at http://{addr}{ENDPOINT_PATH}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_an_ipv4_loopback_address() {
        let addr = parse_listen_addr("127.0.0.1:7391").unwrap();
        assert_eq!(addr.to_string(), "127.0.0.1:7391");
    }

    #[test]
    fn parses_an_ipv6_loopback_address() {
        let addr = parse_listen_addr("[::1]:7391").unwrap();
        assert!(addr.ip().is_loopback());
        assert_eq!(addr.port(), 7391);
    }

    #[test]
    fn trims_surrounding_whitespace() {
        let addr = parse_listen_addr("  127.0.0.1:7391  ").unwrap();
        assert_eq!(addr.port(), 7391);
    }

    #[test]
    fn rejects_an_empty_address() {
        let err = parse_listen_addr("   ").unwrap_err().to_string();
        assert!(err.contains("127.0.0.1:7391"), "got: {err}");
    }

    #[test]
    fn rejects_an_address_with_no_port() {
        let err = parse_listen_addr("127.0.0.1").unwrap_err().to_string();
        assert!(err.contains("host:port"), "got: {err}");
    }

    #[test]
    fn rejects_a_non_loopback_address() {
        let err = parse_listen_addr("0.0.0.0:7391").unwrap_err().to_string();
        assert!(err.contains("loopback"), "got: {err}");
    }

    #[test]
    fn ready_line_names_the_endpoint_path() {
        let line = ready_line("127.0.0.1:7391".parse().unwrap());
        assert_eq!(
            line,
            "CSR: MCP Streamable HTTP ready at http://127.0.0.1:7391/mcp"
        );
    }

    /// Minimal handler: every `ServerHandler` method has a default, so this
    /// exercises the transport without loading an embedding model.
    #[derive(Clone)]
    struct ProbeServer;

    impl ServerHandler for ProbeServer {}

    #[tokio::test]
    async fn completes_an_initialize_handshake_over_http() {
        let listener = bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let _ = serve(ProbeServer, listener).await;
        });

        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "csr-http-test", "version": "0.0.0" }
            }
        });

        let response = reqwest::Client::new()
            .post(format!("http://{addr}{ENDPOINT_PATH}"))
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .json(&body)
            .send()
            .await
            .expect("initialize request should reach the server");

        assert!(
            response.status().is_success(),
            "initialize returned {}",
            response.status()
        );
        assert!(
            response.headers().contains_key("mcp-session-id"),
            "server should hand back a session id"
        );

        let text = response.text().await.unwrap();
        assert!(text.contains("\"protocolVersion\""), "body was: {text}");
        assert!(text.contains("\"serverInfo\""), "body was: {text}");
        assert!(text.contains("\"result\""), "body was: {text}");

        server.abort();
    }

    #[tokio::test]
    async fn rejects_a_post_that_does_not_accept_event_stream() {
        let listener = bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let _ = serve(ProbeServer, listener).await;
        });

        let response = reqwest::Client::new()
            .post(format!("http://{addr}{ENDPOINT_PATH}"))
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .body("{}")
            .send()
            .await
            .expect("request should reach the server");

        assert_eq!(response.status().as_u16(), 406);

        server.abort();
    }

    /// Run `serve_until` on an ephemeral port. Sending on the returned sender
    /// is the shutdown trigger.
    async fn spawn_stoppable() -> (
        String,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<Result<()>>,
    ) {
        let listener = bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let url = format!("http://{}{ENDPOINT_PATH}", listener.local_addr().unwrap());
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(serve_until(ProbeServer, listener, async move {
            let _ = stopped.await;
        }));
        (url, stop, server)
    }

    /// Initialize a session the way a client does before it opens the GET
    /// event stream, and return the session id.
    async fn open_session(client: &reqwest::Client, url: &str) -> String {
        let initialize = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "csr-http-test", "version": "0.0.0" }
            }
        });
        let response = client
            .post(url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .json(&initialize)
            .send()
            .await
            .expect("initialize request should reach the server");
        let session = response
            .headers()
            .get("mcp-session-id")
            .expect("server should hand back a session id")
            .to_str()
            .unwrap()
            .to_string();
        let _ = response.text().await;

        let initialized = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        });
        let response = client
            .post(url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .header("mcp-session-id", &session)
            .header("mcp-protocol-version", "2025-06-18")
            .json(&initialized)
            .send()
            .await
            .expect("initialized notification should reach the server");
        assert!(
            response.status().is_success(),
            "initialized returned {}",
            response.status()
        );
        session
    }

    #[tokio::test]
    async fn shutdown_returns_with_no_clients_connected() {
        let (_url, stop, server) = spawn_stoppable().await;

        stop.send(()).unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(5), server)
            .await
            .expect("serve_until should return once shutdown fires")
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn shutdown_does_not_wait_on_an_open_event_stream() {
        let (url, stop, server) = spawn_stoppable().await;
        let client = reqwest::Client::new();
        let session = open_session(&client, &url).await;

        // The GET stream a connected client keeps open for server-initiated
        // messages. It stays open across the shutdown below.
        let stream = client
            .get(&url)
            .header("accept", "text/event-stream")
            .header("mcp-session-id", &session)
            .header("mcp-protocol-version", "2025-06-18")
            .send()
            .await
            .expect("GET should open the event stream");
        assert!(
            stream.status().is_success(),
            "GET returned {}",
            stream.status()
        );

        stop.send(()).unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(5), server)
            .await
            .expect("an open event stream kept the server from shutting down")
            .unwrap()
            .unwrap();
        drop(stream);
    }
}
