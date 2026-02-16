pub mod resources;
pub mod tools;

use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::service::RequestContext;
use rmcp::{tool, tool_handler, tool_router, RoleServer, ServerHandler};
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

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchInsightsParams {
    /// The search query to find semantically similar conversations
    pub query: String,
    /// Search specific project only, or 'all' for cross-project
    pub project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetRecentWorkParams {
    /// Number of recent conversations to return (default: 10)
    pub limit: Option<usize>,
    /// Specific project or 'all' for cross-project
    pub project: Option<String>,
    /// Include stored reflections (default: true)
    pub include_reflections: Option<bool>,
    /// Group by 'conversation', 'day', or 'session' (default: conversation)
    pub group_by: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchByRecencyParams {
    /// Semantic search query
    pub query: String,
    /// Natural language time like 'last week', 'yesterday'
    pub time_range: Option<String>,
    /// ISO timestamp or relative time
    pub since: Option<String>,
    /// ISO timestamp or relative time
    pub until: Option<String>,
    /// Maximum number of results (default: 10)
    pub limit: Option<usize>,
    /// Minimum similarity score (default: 0.3)
    pub min_score: Option<f32>,
    /// Specific project or 'all'
    pub project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetTimelineParams {
    /// Natural language time range (default: 'last week')
    pub time_range: Option<String>,
    /// Specific project or 'all'
    pub project: Option<String>,
    /// 'hour', 'day', 'week', or 'month' (default: day)
    pub granularity: Option<String>,
    /// Include activity statistics (default: true)
    pub include_stats: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchByFileParams {
    /// The file path to search for in conversations
    pub file_path: String,
    /// Maximum number of results to return (default: 10)
    pub limit: Option<usize>,
    /// Search specific project only, or 'all'
    pub project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchByConceptParams {
    /// The concept to search for (e.g., 'security', 'docker', 'testing')
    pub concept: String,
    /// Maximum number of results to return (default: 10)
    pub limit: Option<usize>,
    /// Search specific project only, or 'all'
    pub project: Option<String>,
    /// Include file information in results (default: true)
    pub include_files: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetMoreResultsParams {
    /// The original search query
    pub query: String,
    /// Number of results to skip for pagination (default: 3)
    pub offset: Option<usize>,
    /// Number of additional results to return (default: 3)
    pub limit: Option<usize>,
    /// Minimum similarity score 0-1 (default: 0.3)
    pub min_score: Option<f32>,
    /// Search specific project only
    pub project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetFullConversationParams {
    /// The conversation ID from search results (cid)
    pub conversation_id: String,
    /// Optional project name to help locate the file
    pub project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetSessionLearningsParams {
    /// Ralph session ID to get learnings from
    pub session_id: String,
    /// Maximum number of learnings to return (default: 50)
    pub limit: Option<usize>,
}

// ─── MCP Server ───

/// The MCP server that exposes CSR search/reflection tools.
#[derive(Clone)]
pub struct CsrServer {
    storage: Arc<Storage>,
    embeddings: Arc<EmbeddingEngine>,
    search: Arc<RwLock<SearchEngine>>,
    projects_dir: PathBuf,
    index_dir: PathBuf,
    db_path: String,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl CsrServer {
    pub fn new(
        storage: Arc<Storage>,
        embeddings: Arc<EmbeddingEngine>,
        search: Arc<RwLock<SearchEngine>>,
        projects_dir: PathBuf,
        index_dir: PathBuf,
    ) -> Self {
        // Derive db_path from index_dir (sibling: index_dir/../csr-engine.db)
        let db_path = index_dir
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join("csr-engine.db")
            .to_string_lossy()
            .to_string();
        Self {
            storage,
            embeddings,
            search,
            projects_dir,
            index_dir,
            db_path,
            tool_router: Self::tool_router(),
        }
    }

    /// Flush the HNSW index to disk if dirty.
    async fn flush_index(&self) {
        let mut idx = self.search.write().await;
        if idx.is_dirty() {
            let chunk_count = self.storage.count_chunk_embeddings().unwrap_or(0);
            let refl_count = self.storage.count_reflection_embeddings().unwrap_or(0);
            if let Err(e) = idx.dump_to_disk(&self.index_dir, chunk_count, refl_count) {
                tracing::warn!(error = %e, "failed to flush HNSW index after store_reflection");
            }
        }
    }

    // ─── Core tools (3) ───

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
            p.project.as_deref(),
        )
        .await;

        tool_result(result)
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

        // Flush index to disk so the new reflection persists for other processes (H-3)
        self.flush_index().await;

        tool_result(result)
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

        tool_result(result)
    }

    // ─── Insights & aggregation (1) ───

    #[tool(
        name = "csr_search_insights",
        description = "Get aggregated insights and patterns from search results. Use when you want patterns or trends, analyzing topic evolution, understanding common themes."
    )]
    async fn search_insights(
        &self,
        params: Parameters<SearchInsightsParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let p = params.0;
        let result = tools::search_insights(
            &self.storage,
            &self.embeddings,
            &self.search,
            &p.query,
            p.project.as_deref(),
        )
        .await;

        tool_result(result)
    }

    // ─── Temporal tools (3) ───

    #[tool(
        name = "get_recent_work",
        description = "Get recent work conversations to answer 'What did we work on last?' queries."
    )]
    async fn get_recent_work(
        &self,
        params: Parameters<GetRecentWorkParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let p = params.0;
        let limit = p.limit.unwrap_or(10);
        let group_by = p.group_by.as_deref().unwrap_or("conversation");

        let result = tools::get_recent_work(
            &self.storage,
            limit,
            p.project.as_deref(),
            group_by,
        )
        .await;

        tool_result(result)
    }

    #[tool(
        name = "search_by_recency",
        description = "Time-constrained semantic search for queries like 'docker issues last week'."
    )]
    async fn search_by_recency(
        &self,
        params: Parameters<SearchByRecencyParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let p = params.0;
        let limit = p.limit.unwrap_or(10);
        let min_score = p.min_score.unwrap_or(0.3);

        let result = tools::search_by_recency(
            &self.storage,
            &self.embeddings,
            &self.search,
            &p.query,
            p.time_range.as_deref(),
            p.since.as_deref(),
            p.until.as_deref(),
            limit,
            min_score,
            p.project.as_deref(),
        )
        .await;

        tool_result(result)
    }

    #[tool(
        name = "get_timeline",
        description = "Show activity timeline for a project or across all projects."
    )]
    async fn get_timeline(
        &self,
        params: Parameters<GetTimelineParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let p = params.0;
        let time_range = p.time_range.as_deref().unwrap_or("last week");
        let granularity = p.granularity.as_deref().unwrap_or("day");

        let result = tools::get_timeline(
            &self.storage,
            time_range,
            p.project.as_deref(),
            granularity,
        )
        .await;

        tool_result(result)
    }

    // ─── File & concept search (2) ───

    #[tool(
        name = "csr_search_by_file",
        description = "Find all conversations that analyzed or modified a specific file. Perfect for code archaeology and understanding file evolution."
    )]
    async fn search_by_file(
        &self,
        params: Parameters<SearchByFileParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let p = params.0;
        let limit = p.limit.unwrap_or(10);

        let result = tools::search_by_file(
            &self.storage,
            &p.file_path,
            limit,
            p.project.as_deref(),
        )
        .await;

        tool_result(result)
    }

    #[tool(
        name = "csr_search_by_concept",
        description = "Search for conversations about specific development concepts or themes like 'security', 'testing', 'performance'."
    )]
    async fn search_by_concept(
        &self,
        params: Parameters<SearchByConceptParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let p = params.0;
        let limit = p.limit.unwrap_or(10);

        let result = tools::search_by_concept(
            &self.storage,
            &self.embeddings,
            &self.search,
            &p.concept,
            limit,
            p.project.as_deref(),
        )
        .await;

        tool_result(result)
    }

    // ─── Pagination (1) ───

    #[tool(
        name = "csr_get_more",
        description = "Get additional search results for paginated exploration. Use after initial search when more context is needed."
    )]
    async fn get_more_results(
        &self,
        params: Parameters<GetMoreResultsParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let p = params.0;
        let offset = p.offset.unwrap_or(3);
        let limit = p.limit.unwrap_or(3);
        let min_score = p.min_score.unwrap_or(0.3);

        let result = tools::get_more_results(
            &self.storage,
            &self.embeddings,
            &self.search,
            &p.query,
            offset,
            limit,
            min_score,
            p.project.as_deref(),
        )
        .await;

        tool_result(result)
    }

    // ─── Conversation & session (2) ───

    #[tool(
        name = "get_full_conversation",
        description = "Get the full JSONL conversation file path for a conversation ID. This allows reading complete conversations instead of truncated excerpts."
    )]
    async fn get_full_conversation(
        &self,
        params: Parameters<GetFullConversationParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let p = params.0;
        let text = tools::get_full_conversation(
            &self.projects_dir,
            &p.conversation_id,
            p.project.as_deref(),
        );
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(
        name = "get_session_learnings",
        description = "Get all learnings from a specific Ralph session. Enables iteration-level memory across context resets."
    )]
    async fn get_session_learnings(
        &self,
        params: Parameters<GetSessionLearningsParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let p = params.0;
        let limit = p.limit.unwrap_or(50);

        let result = tools::get_session_learnings(&self.storage, &p.session_id, limit);
        tool_result(result)
    }
}

/// Helper to convert Result<String> to tool result.
fn tool_result(result: anyhow::Result<String>) -> Result<CallToolResult, rmcp::ErrorData> {
    match result {
        Ok(text) => Ok(CallToolResult::success(vec![Content::text(text)])),
        Err(e) => Err(rmcp::ErrorData::internal_error(
            format!("{e}"),
            None,
        )),
    }
}

#[tool_handler]
impl ServerHandler for CsrServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "Claude Self-Reflect: Search past conversations and store reflections with semantic search and time-based memory decay.".into(),
            ),
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
            server_info: Implementation {
                name: "csr-engine".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListResourcesResult, rmcp::ErrorData>> + Send + '_ {
        async {
            let mut health = RawResource::new("status://system-health", "System Health");
            health.description = Some("Current system health: index stats, cache status, version".into());
            health.mime_type = Some("application/json".into());

            Ok(ListResourcesResult {
                resources: vec![health.no_annotation()],
                next_cursor: None,
                meta: None,
            })
        }
    }

    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ReadResourceResult, rmcp::ErrorData>> + Send + '_ {
        async move {
            match request.uri.as_str() {
                "status://system-health" => {
                    let text = resources::system_health(
                        &self.storage,
                        &self.search,
                        &self.db_path,
                        &self.index_dir.to_string_lossy(),
                    )
                    .await;
                    Ok(ReadResourceResult {
                        contents: vec![ResourceContents::text(text, &request.uri)],
                    })
                }
                _ => Err(rmcp::ErrorData::resource_not_found(
                    format!("Unknown resource: {}", request.uri),
                    None,
                )),
            }
        }
    }
}
