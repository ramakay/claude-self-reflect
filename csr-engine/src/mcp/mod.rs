pub mod completions;
pub mod elicitation;
pub mod resources;
pub mod tasks;
pub mod tools;

use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

use rmcp::handler::server::tool::ToolCallContext;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::service::RequestContext;
use rmcp::task_manager::{TaskExit, TaskManager, TaskOptions};
use rmcp::{tool, tool_router, RoleServer, ServerHandler};
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
    /// Session ID to get learnings from
    pub session_id: String,
    /// Maximum number of learnings to return (default: 50)
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CodeGraphParams {
    /// Symbol name to query (function/type), e.g. 'dispatch_hook'
    pub symbol: Option<String>,
    /// File path to anchor the query when no symbol is given
    pub file: Option<String>,
    /// Query mode: 'neighbors' (default), 'callers', or 'callees'
    pub mode: Option<String>,
    /// Traversal depth (currently 1-hop; reserved)
    pub depth: Option<u32>,
    /// Maximum number of results (default: 20)
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WhyParams {
    /// The "why/how did this come to be" question.
    pub query: String,
    /// Max evidence items (default 10, capped at 50).
    pub limit: Option<usize>,
    /// Project scope (same semantics as csr_reflect_on_past).
    pub project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ResolveParams {
    /// Chunk ids (from <id> tags in search results — NOT <cid>, which is the conversation id) this verdict applies to
    pub chunk_ids: Vec<String>,
    /// One of: resolved (verified addressed), still_open (verified still pending), regressed (previously resolved, broke again)
    pub status: String,
    /// What was verified and how, e.g. 'shipped vc75 commit 332ef68, confirmed in app.json'
    pub evidence: String,
    /// Optional short digest of the claim being resolved
    pub claim: Option<String>,
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
    task_manager: TaskManager,
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
            task_manager: TaskManager::new(),
        }
    }

    /// Number of tools the rmcp router exposes — the eval suite asserts on this
    /// so the count can't silently drift from what docs claim.
    pub fn tool_count() -> usize {
        Self::tool_router().list_all().len()
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
        description = "Search past Claude conversations semantically to find relevant context. Use this tool when you need to recall previous discussions, find solutions to problems encountered before, or understand project history.",
        annotations(
            title = "Reflect on Past",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        )
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
        description = "Store an important insight or reflection for future reference. Use this to save key decisions, solutions, patterns, or warnings that should be remembered across sessions.",
        annotations(
            title = "Store Reflection",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false
        )
    )]
    async fn store_reflection(
        &self,
        params: Parameters<StoreReflectionParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let p = params.0;
        let tags = p.tags.unwrap_or_default();

        // Elicitation: confirm before storing large reflections
        if elicitation::needs_confirmation(&p.content)
            && !elicitation::request_confirmation(&p.content, &context).await
        {
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                "Reflection storage declined by user.",
            )]));
        }

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
        description = "Quick check if a topic was discussed before. Returns count and top match only. Much faster than full search - use for existence checks. Answers negatively: when nothing scores above the relevance floor it returns <found>false</found> with no preview, and matches in the weak band are labelled as possibly spurious.",
        annotations(
            title = "Quick Check",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        )
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
            p.project.as_deref(),
        )
        .await;

        tool_result(result)
    }

    // ─── Insights & aggregation (1) ───

    #[tool(
        name = "csr_search_insights",
        description = "Get aggregated insights and patterns from search results. Use when you want patterns or trends, analyzing topic evolution, understanding common themes.",
        annotations(
            title = "Search Insights",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        )
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
        description = "Get recent work conversations to answer 'What did we work on last?' queries.",
        annotations(
            title = "Get Recent Work",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn get_recent_work(
        &self,
        params: Parameters<GetRecentWorkParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let p = params.0;
        let limit = p.limit.unwrap_or(10);
        let group_by = p.group_by.as_deref().unwrap_or("conversation");

        let result =
            tools::get_recent_work(&self.storage, limit, p.project.as_deref(), group_by).await;

        tool_result(result)
    }

    #[tool(
        name = "search_by_recency",
        description = "Time-constrained semantic search for queries like 'docker issues last week'.",
        annotations(
            title = "Search by Recency",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        )
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
        description = "Show activity timeline for a project or across all projects.",
        annotations(
            title = "Get Timeline",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn get_timeline(
        &self,
        params: Parameters<GetTimelineParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let p = params.0;
        let time_range = p.time_range.as_deref().unwrap_or("last week");
        let granularity = p.granularity.as_deref().unwrap_or("day");

        let result =
            tools::get_timeline(&self.storage, time_range, p.project.as_deref(), granularity).await;

        tool_result(result)
    }

    // ─── File & concept search (2) ───

    #[tool(
        name = "csr_search_by_file",
        description = "Find all conversations that analyzed or modified a specific file. Perfect for code archaeology and understanding file evolution.",
        annotations(
            title = "Search by File",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn search_by_file(
        &self,
        params: Parameters<SearchByFileParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let p = params.0;
        let limit = p.limit.unwrap_or(10);

        let result =
            tools::search_by_file(&self.storage, &p.file_path, limit, p.project.as_deref()).await;

        tool_result(result)
    }

    #[tool(
        name = "csr_search_by_concept",
        description = "Search for conversations about specific development concepts or themes like 'security', 'testing', 'performance'.",
        annotations(
            title = "Search by Concept",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        )
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
        description = "Get additional search results for paginated exploration. Use after initial search when more context is needed.",
        annotations(
            title = "Get More Results",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        )
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
        description = "Get the full JSONL conversation file path for a conversation ID. This allows reading complete conversations instead of truncated excerpts.",
        annotations(
            title = "Get Full Conversation",
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = true
        )
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
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }

    #[tool(
        name = "get_session_learnings",
        description = "Get all learnings from a specific session. Enables iteration-level memory across context resets.",
        annotations(
            title = "Get Session Learnings",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        )
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

    // ─── Code property graph (1) ───

    #[tool(
        name = "csr_code_graph",
        description = "Query the conversation-provenance code graph: who calls a symbol (callers), what it calls (callees), or its 1-hop neighbors. Each result carries the conversation that last changed it. Modes: neighbors|callers|callees.",
        annotations(
            title = "Code Graph",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn code_graph(
        &self,
        params: Parameters<CodeGraphParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let p = params.0;
        let mode = p.mode.as_deref().unwrap_or("neighbors");
        let limit = p.limit.unwrap_or(20) as usize;

        let result = tools::code_graph(
            &self.storage,
            p.symbol.as_deref(),
            p.file.as_deref(),
            mode,
            limit,
        )
        .await;

        tool_result(result)
    }

    // ─── Provenance recall (1) ───

    #[tool(
        name = "csr_why",
        description = "Provenance chain: why does this code/decision exist. Reinstatement recall — seed retrieval, blended re-query, code-graph spread, and episode-chain hops surface the conversations that originated a decision, not just the ones that mention it.",
        annotations(
            title = "Why",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn why(&self, params: Parameters<WhyParams>) -> Result<CallToolResult, rmcp::ErrorData> {
        let p = params.0;
        let limit = p.limit.unwrap_or(10).min(50);
        let cfg = crate::search::reinstatement::ReinstateConfig {
            k: limit,
            ..Default::default()
        };

        let result = tools::why(
            &self.storage,
            &self.embeddings,
            &self.search,
            &p.query,
            p.project.as_deref(),
            &cfg,
        )
        .await;

        tool_result(result)
    }

    #[tool(
        name = "csr_resolve",
        description = "Record an explicit verdict (resolved/still_open/regressed) about chunks surfaced in search results, verified against the repo or real world. Future searches annotate these chunks and demote resolved ones within the page. Verdict applies to the WHOLE chunk — for multi-claim chunks resolve only when all claims are addressed, otherwise use still_open. Append-only: a regressed verdict re-opens a resolved chunk.",
        annotations(
            title = "Record Resolution Verdict",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false
        )
    )]
    async fn resolve(
        &self,
        params: Parameters<ResolveParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let p = params.0;

        let result =
            tools::resolve_chunks(&self.storage, p.chunk_ids, p.status, p.evidence, p.claim).await;

        tool_result(result)
    }
}

/// Helper to convert Result<String> to tool result.
fn tool_result(result: anyhow::Result<String>) -> Result<CallToolResult, rmcp::ErrorData> {
    match result {
        Ok(text) => Ok(CallToolResult::success(vec![ContentBlock::text(text)])),
        Err(e) => Err(rmcp::ErrorData::internal_error(format!("{e}"), None)),
    }
}

impl ServerHandler for CsrServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_completions()
                .enable_tasks()
                .build(),
        )
        .with_instructions(
            "Claude Self-Reflect: Search past conversations and store reflections with semantic search and time-based memory decay.",
        )
        .with_server_info(Implementation::new(
            "csr-engine",
            env!("CARGO_PKG_VERSION"),
        ))
    }

    async fn complete(
        &self,
        request: CompleteRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CompleteResult, rmcp::ErrorData> {
        completions::handle_complete(&request, &self.storage)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, rmcp::ErrorData> {
        Ok(ListToolsResult::with_all_items(
            Self::tool_router().list_all(),
        ))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, rmcp::ErrorData> {
        let client_supports_tasks = context
            .client_capabilities()
            .is_some_and(|capabilities| capabilities.supports_tasks());

        if tasks::is_taskable(request.name.as_ref()) && client_supports_tasks {
            let server = self.clone();
            let task_options = TaskOptions::default()
                .with_ttl_ms(30_000)
                .with_poll_interval_ms(500)
                .with_status_message(format!("Running {}", request.name));
            let task = self.task_manager.spawn(task_options, move |task_context| {
                Box::pin(async move {
                    let tool_call = ToolCallContext::new(&server, request, context);
                    let tool_router = CsrServer::tool_router();
                    tokio::select! {
                        _ = task_context.cancelled() => Err(TaskExit::Cancelled),
                        result = tool_router.call(tool_call) => match result {
                            Ok(CallToolResponse::Complete(result)) => Ok(result),
                            Ok(_) => Err(TaskExit::Error(rmcp::ErrorData::internal_error(
                                "Tool returned an unsupported response while executing as a task",
                                None,
                            ))),
                            Err(error) => Err(TaskExit::Error(error)),
                        }
                    }
                })
            });
            return Ok(CallToolResponse::Task(CreateTaskResult::new(task)));
        }

        let tool_call = ToolCallContext::new(self, request, context);
        Self::tool_router().call(tool_call).await
    }

    async fn get_task(
        &self,
        request: GetTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetTaskResult, rmcp::ErrorData> {
        Ok(GetTaskResult::new(
            self.task_manager.get_task(&request.task_id)?,
        ))
    }

    async fn update_task(
        &self,
        request: UpdateTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), rmcp::ErrorData> {
        self.task_manager
            .update_task(&request.task_id, request.input_responses)
    }

    async fn cancel_task(
        &self,
        request: CancelTaskParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), rmcp::ErrorData> {
        self.task_manager.cancel_task(&request.task_id)
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, rmcp::ErrorData> {
        let health = Resource::new("status://system-health", "System Health")
            .with_description("Current system health: index stats, cache status, version")
            .with_mime_type("application/json");

        Ok(ListResourcesResult::with_all_items(vec![health]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, rmcp::ErrorData> {
        match request.uri.as_str() {
            "status://system-health" => {
                let text = resources::system_health(
                    &self.storage,
                    &self.search,
                    &self.db_path,
                    &self.index_dir.to_string_lossy(),
                )
                .await;
                Ok(
                    ReadResourceResult::new(vec![ResourceContents::text(text, &request.uri)])
                        .into(),
                )
            }
            _ => Err(rmcp::ErrorData::resource_not_found(
                format!("Unknown resource: {}", request.uri),
                None,
            )),
        }
    }
}
