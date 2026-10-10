//! Pre-handshake screen for protocol-version probes on the stdio transport.
//!
//! Claude Code opens stdio connections with a `server/discover` probe (the
//! 2026-07-28 stateless lifecycle) that carries the full per-request `_meta`
//! block. When the server answers `-32022 Unsupported protocol version`, the
//! client sends a legacy `initialize` on the same process (observed on the
//! wire with 2.1.288; the failure below first shows in MCP logs with 2.1.286).
//!
//! rmcp 3.1 (the logic is unchanged through 3.5) treats any non-`initialize`
//! first request with a complete `_meta` as an inline-lifecycle opener and sets
//! a sticky, session-wide "request metadata required" flag before it dispatches
//! that request. CsrServer caps negotiation below 2026-07-28, so the dispatch
//! fails with -32022 and the client falls back to `initialize`, which succeeds.
//! The flag survives, though: every later legacy request without `_meta`
//! (`tools/list`, `resources/list`) is rejected with -32602 "request _meta is
//! missing or has malformed required fields". The server is connected but has
//! no tools.
//!
//! [`LegacyOpenerGuard`] answers such a probe itself with the same -32022 error
//! rmcp would have sent, and never forwards it. rmcp's handshake then sees
//! `initialize` as its first message and runs an ordinary legacy session.
//! Every other message passes through untouched, and nothing is screened once
//! the session has opened.

use std::borrow::Cow;
use std::future::Future;

use rmcp::model::{
    ClientJsonRpcMessage, ClientRequest, ErrorData, GetMeta, JsonRpcMessage, ProtocolVersion,
    ServerJsonRpcMessage,
};
use rmcp::service::{RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::Transport;
use rmcp::RoleServer;

/// Wraps a server transport and screens pre-handshake requests whose `_meta`
/// asks for a protocol revision the server does not support.
pub struct LegacyOpenerGuard<T> {
    inner: T,
    supported: Cow<'static, [ProtocolVersion]>,
    opened: bool,
}

enum Screened {
    /// Hand the message to rmcp unchanged.
    Forward(ClientJsonRpcMessage),
    /// Answer the client directly; rmcp never sees the request.
    Reject(ServerJsonRpcMessage),
}

impl<T> LegacyOpenerGuard<T> {
    /// `supported` must be the list the server negotiates with
    /// (`ServerHandler::supported_protocol_versions`), so the guard rejects
    /// exactly the probes rmcp would have rejected.
    pub fn new(inner: T, supported: Cow<'static, [ProtocolVersion]>) -> Self {
        Self {
            inner,
            supported,
            opened: false,
        }
    }

    fn screen(&mut self, message: ClientJsonRpcMessage) -> Screened {
        if self.opened {
            return Screened::Forward(message);
        }
        let rejection = match &message {
            JsonRpcMessage::Request(request) => match &request.request {
                // rmcp answers pre-init pings itself; they do not open a session.
                ClientRequest::PingRequest(_) => None,
                ClientRequest::InitializeRequest(_) => {
                    self.opened = true;
                    None
                }
                other => match other.get_meta().protocol_version() {
                    Some(requested) if !self.supported.contains(&requested) => {
                        tracing::debug!(
                            requested = %requested,
                            "answering pre-handshake probe for an unsupported protocol \
                             version; waiting for initialize"
                        );
                        Some(ServerJsonRpcMessage::error(
                            ErrorData::unsupported_protocol_version(requested, &self.supported),
                            Some(request.id.clone()),
                        ))
                    }
                    // A supported inline opener, or one without a usable
                    // `_meta`: rmcp's own handshake decides what happens.
                    _ => {
                        self.opened = true;
                        None
                    }
                },
            },
            // Notifications and responses before `initialize` stay rmcp's call.
            _ => None,
        };
        match rejection {
            Some(reply) => Screened::Reject(reply),
            None => Screened::Forward(message),
        }
    }
}

impl<T> Transport<RoleServer> for LegacyOpenerGuard<T>
where
    T: Transport<RoleServer>,
{
    type Error = T::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleServer>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        self.inner.send(item)
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleServer>> {
        loop {
            let message = self.inner.receive().await?;
            match self.screen(message) {
                Screened::Forward(message) => return Some(message),
                Screened::Reject(reply) => {
                    if let Err(error) = self.inner.send(reply).await {
                        tracing::warn!(
                            error = %error,
                            "failed to answer pre-handshake protocol probe"
                        );
                        return None;
                    }
                }
            }
        }
    }

    fn close(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        self.inner.close()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CC_META: &str = r#"{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientInfo":{"name":"claude-code","title":"Claude Code","version":"2.1.288","description":"Anthropic's agentic coding tool","websiteUrl":"https://claude.com/claude-code"},"io.modelcontextprotocol/clientCapabilities":{"roots":{"listChanged":true},"elicitation":{"form":{},"url":{}}}}"#;

    /// The exact `server/discover` probe Claude Code 2.1.288 opens with.
    fn discover_probe() -> String {
        format!(
            r#"{{"jsonrpc":"2.0","id":"server-discover-probe-1","method":"server/discover","params":{{"_meta":{CC_META}}}}}"#
        )
    }

    /// The legacy `initialize` Claude Code 2.1.288 falls back to.
    const CC_INITIALIZE: &str = r#"{"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{"roots":{"listChanged":true},"elicitation":{"form":{},"url":{}}},"clientInfo":{"name":"claude-code","title":"Claude Code","version":"2.1.288","description":"Anthropic's agentic coding tool","websiteUrl":"https://claude.com/claude-code"}},"jsonrpc":"2.0","id":0}"#;
    const CC_INITIALIZED: &str = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    const CC_TOOLS_LIST: &str = r#"{"method":"tools/list","jsonrpc":"2.0","id":1}"#;
    const CC_RESOURCES_LIST: &str = r#"{"method":"resources/list","jsonrpc":"2.0","id":2}"#;

    fn legacy_only() -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(&[
            ProtocolVersion::V_2024_11_05,
            ProtocolVersion::V_2025_03_26,
            ProtocolVersion::V_2025_06_18,
            ProtocolVersion::V_2025_11_25,
        ])
    }

    fn parse(line: &str) -> ClientJsonRpcMessage {
        serde_json::from_str(line).expect("valid client message")
    }

    fn guard() -> LegacyOpenerGuard<()> {
        LegacyOpenerGuard::new((), legacy_only())
    }

    #[test]
    fn unsupported_discover_probe_is_answered_not_forwarded() {
        let mut guard = guard();
        let Screened::Reject(reply) = guard.screen(parse(&discover_probe())) else {
            panic!("an unsupported-version probe must not reach rmcp");
        };
        let reply = serde_json::to_value(&reply).unwrap();
        assert_eq!(reply["id"], "server-discover-probe-1");
        assert_eq!(reply["error"]["code"], -32022);
        assert_eq!(reply["error"]["data"]["requested"], "2026-07-28");
        assert_eq!(
            reply["error"]["data"]["supported"],
            serde_json::json!(["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"])
        );
        assert!(!guard.opened, "a rejected probe must not open the session");
    }

    #[test]
    fn initialize_opens_and_later_messages_pass_untouched() {
        let mut guard = guard();
        assert!(matches!(
            guard.screen(parse(&discover_probe())),
            Screened::Reject(_)
        ));
        assert!(matches!(
            guard.screen(parse(CC_INITIALIZE)),
            Screened::Forward(_)
        ));
        assert!(guard.opened);
        // After the handshake rmcp's own per-request checks apply, so even an
        // unsupported-version request is forwarded.
        assert!(matches!(
            guard.screen(parse(&discover_probe())),
            Screened::Forward(_)
        ));
        assert!(matches!(
            guard.screen(parse(CC_TOOLS_LIST)),
            Screened::Forward(_)
        ));
    }

    #[test]
    fn pre_init_ping_passes_without_opening() {
        let mut guard = guard();
        let ping = r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#;
        assert!(matches!(guard.screen(parse(ping)), Screened::Forward(_)));
        assert!(!guard.opened);
    }

    #[test]
    fn supported_inline_opener_is_left_to_rmcp() {
        let mut guard = guard();
        let probe = discover_probe().replace("2026-07-28", "2025-11-25");
        assert!(matches!(guard.screen(parse(&probe)), Screened::Forward(_)));
        assert!(guard.opened);
    }

    #[test]
    fn opener_without_meta_is_left_to_rmcp() {
        let mut guard = guard();
        assert!(matches!(
            guard.screen(parse(CC_TOOLS_LIST)),
            Screened::Forward(_)
        ));
        assert!(guard.opened);
    }

    // ─── End to end: a real CsrServer over an in-process duplex, driven with
    // the exact JSON-RPC lines Claude Code 2.1.288 writes ───

    use std::sync::Arc;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    fn csr_server(dir: &std::path::Path) -> crate::mcp::CsrServer {
        crate::mcp::CsrServer::new(
            Arc::new(crate::storage::Storage::open_memory().unwrap()),
            Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap()),
            Arc::new(tokio::sync::RwLock::new(crate::search::SearchEngine::new(
                100,
            ))),
            dir.to_path_buf(),
            dir.to_path_buf(),
            None,
            None,
        )
    }

    /// Writes each client line and collects the server's reply to every
    /// request (lines with an `id`).
    async fn drive(client: tokio::io::DuplexStream, script: &[String]) -> Vec<serde_json::Value> {
        let (read, mut write) = tokio::io::split(client);
        let mut lines = BufReader::new(read).lines();
        let mut replies = Vec::new();
        for line in script {
            write.write_all(line.as_bytes()).await.unwrap();
            write.write_all(b"\n").await.unwrap();
            write.flush().await.unwrap();
            let sent: serde_json::Value = serde_json::from_str(line).unwrap();
            if sent.get("id").is_none() {
                continue;
            }
            let reply = tokio::time::timeout(std::time::Duration::from_secs(30), lines.next_line())
                .await
                .expect("server replied in time")
                .unwrap()
                .expect("server kept the connection open");
            replies.push(serde_json::from_str(&reply).unwrap());
        }
        replies
    }

    fn claude_code_2_1_288_script() -> Vec<String> {
        vec![
            discover_probe(),
            CC_INITIALIZE.to_string(),
            CC_INITIALIZED.to_string(),
            CC_TOOLS_LIST.to_string(),
            CC_RESOURCES_LIST.to_string(),
        ]
    }

    #[tokio::test]
    async fn claude_code_discover_then_initialize_session_serves_every_tool() {
        let tmp = tempfile::tempdir().unwrap();
        let server = csr_server(tmp.path());
        let (server_io, client_io) = tokio::io::duplex(1 << 20);
        let server_task = tokio::spawn(async move {
            let (read, write) = tokio::io::split(server_io);
            if let Ok(running) = server.serve_io(read, write).await {
                let _ = running.waiting().await;
            }
        });

        let replies = drive(client_io, &claude_code_2_1_288_script()).await;
        server_task.abort();

        assert_eq!(replies[0]["id"], "server-discover-probe-1");
        assert_eq!(replies[0]["error"]["code"], -32022);
        assert_eq!(replies[1]["result"]["protocolVersion"], "2025-11-25");
        let tools = replies[2]["result"]["tools"]
            .as_array()
            .unwrap_or_else(|| panic!("tools/list failed: {}", replies[2]));
        assert_eq!(tools.len(), crate::mcp::CsrServer::tool_count());
        assert!(tools.iter().any(|t| t["name"] == "csr_reflect_on_past"));
        assert!(
            replies[3]["result"]["resources"].is_array(),
            "resources/list failed: {}",
            replies[3]
        );
    }

    /// Pins the rmcp behaviour the guard exists for. If this starts failing
    /// after an rmcp upgrade, rmcp clears the sticky flag on its own and the
    /// guard can be retired.
    #[tokio::test]
    async fn without_the_guard_rmcp_rejects_tools_list_after_the_probe() {
        use rmcp::ServiceExt;

        let tmp = tempfile::tempdir().unwrap();
        let server = csr_server(tmp.path());
        let (server_io, client_io) = tokio::io::duplex(1 << 20);
        let server_task = tokio::spawn(async move {
            let (read, write) = tokio::io::split(server_io);
            let transport = rmcp::transport::async_rw::AsyncRwTransport::new_server(read, write);
            if let Ok(running) = server.serve(transport).await {
                let _ = running.waiting().await;
            }
        });

        let replies = drive(client_io, &claude_code_2_1_288_script()).await;
        server_task.abort();

        assert_eq!(replies[2]["error"]["code"], -32602, "got: {}", replies[2]);
        assert!(replies[2]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("request _meta is missing"));
    }
}
