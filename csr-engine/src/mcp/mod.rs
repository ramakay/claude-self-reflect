pub mod tools;

use std::sync::Arc;
use tokio::sync::RwLock;

use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::{tool, tool_handler, tool_router, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::embeddings::EmbeddingEngine;
use crate::search::SearchEngine;
use crate::storage::Storage;

// ─── Parameter structs ───

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReflectOnPastParams {
    /// The search query to find semantically similar conversations
    pub query: String,
    /// Maximum number of results to return (default: 5)
    pub limit: Option<usize>,
    /// Minimum similarity score 0-1 (default: 0.3)
    pub min_score: Option<f32>,
    /// Search specific project only, or 'all' for cross-project
    pub project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StoreReflectionParams {
    /// The insight or reflection to store
    pub content: String,
    /// Tags to categorize this reflection
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct QuickCheckParams {
    /// The search query to find semantically similar conversations
    pub query: String,
    /// Minimum similarity score 0-1 (default: 0.3)
    pub min_score: Option<f32>,
    /// Search specific project only, or 'all' for cross-project
    pub project: Option<String>,
}

// ─── MCP Server ───

/// The MCP server that exposes CSR search/reflection tools.
#[derive(Clone)]
pub struct CsrServer {
    storage: Arc<Storage>,
    embeddings: Arc<EmbeddingEngine>,
    search: Arc<RwLock<SearchEngine>>,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl CsrServer {
    pub fn new(
        storage: Arc<Storage>,
        embeddings: Arc<EmbeddingEngine>,
        search: Arc<RwLock<SearchEngine>>,
    ) -> Self {
        Self {
            storage,
            embeddings,
            search,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        name = "csr_reflect_on_past",
        description = "Search past Claude conversations semantically to find relevant context. Use this tool when you need to recall previous discussions, find solutions to problems encountered before, or understand project history."
    )]
    async fn reflect_on_past(
        &self,
        params: Parameters<ReflectOnPastParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let p = params.0;
        let limit = p.limit.unwrap_or(5);
        let min_score = p.min_score.unwrap_or(0.3);

        let result = tools::reflect_on_past(
            &self.storage,
            &self.embeddings,
            &self.search,
            &p.query,
            limit,
            min_score,
        )
        .await;

        match result {
            Ok(text) => Ok(CallToolResult::success(vec![Content::text(text)])),
            Err(e) => Err(rmcp::ErrorData::internal_error(
                format!("Search failed: {e}"),
                None,
            )),
        }
    }

    #[tool(
        name = "store_reflection",
        description = "Store an important insight or reflection for future reference. Use this to save key decisions, solutions, patterns, or warnings that should be remembered across sessions."
    )]
    async fn store_reflection(
        &self,
        params: Parameters<StoreReflectionParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let p = params.0;
        let tags = p.tags.unwrap_or_default();

        let result = tools::store_reflection(
            &self.storage,
            &self.embeddings,
            &self.search,
            &p.content,
            &tags,
        )
        .await;

        match result {
            Ok(text) => Ok(CallToolResult::success(vec![Content::text(text)])),
            Err(e) => Err(rmcp::ErrorData::internal_error(
                format!("Store failed: {e}"),
                None,
            )),
        }
    }

    #[tool(
        name = "csr_quick_check",
        description = "Quick check if a topic was discussed before. Returns count and top match only. Much faster than full search - use for existence checks."
    )]
    async fn quick_check(
        &self,
        params: Parameters<QuickCheckParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let p = params.0;
        let min_score = p.min_score.unwrap_or(0.3);

        let result = tools::quick_check(
            &self.storage,
            &self.embeddings,
            &self.search,
            &p.query,
            min_score,
        )
        .await;

        match result {
            Ok(text) => Ok(CallToolResult::success(vec![Content::text(text)])),
            Err(e) => Err(rmcp::ErrorData::internal_error(
                format!("Quick check failed: {e}"),
                None,
            )),
        }
    }
}

#[tool_handler]
impl ServerHandler for CsrServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "Claude Self-Reflect: Search past conversations and store reflections with semantic search and time-based memory decay.".into(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation {
                name: "csr-engine".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                ..Default::default()
            },
            ..Default::default()
        }
    }
}
