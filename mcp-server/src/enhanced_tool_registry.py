"""Enhanced tool registry with improved descriptions for better tool selection.

This module provides enhanced tool registration with:
1. csr_ namespace prefix for all tools
2. Explicit "when to use" guidance in descriptions
3. Response format flexibility (concise/detailed)
4. Better tool grouping and discoverability
"""

from typing import Optional, List, Literal
from fastmcp import Context
from pydantic import Field


def register_enhanced_search_tools(mcp, tools):
    """Register search tools with enhanced descriptions for better selection rates."""

    # Primary search tool - most commonly needed
    @mcp.tool(name="csr_reflect_on_past")
    async def csr_reflect_on_past(
        ctx: Context,
        query: str = Field(
            description="The search query to find semantically similar conversations"
        ),
        limit: int = Field(
            default=5,
            description="Maximum number of results to return"
        ),
        min_score: float = Field(
            default=0.3,
            description="Minimum similarity score (0-1)"
        ),
        use_decay: int = Field(
            default=-1,
            description="Apply time-based decay: 1=enable, 0=disable, -1=use environment default"
        ),
        project: Optional[str] = Field(
            default=None,
            description="Search specific project only. Use 'all' to search across all projects"
        ),
        mode: str = Field(
            default="full",
            description="Search mode: 'full' (all results), 'quick' (count only), 'summary' (insights)"
        ),
        response_format: Literal["concise", "detailed", "xml"] = Field(
            default="xml",
            description="Output format: 'concise' for brief results, 'detailed' for full context, 'xml' for structured"
        )
    ) -> str:
        """Search past Claude conversations semantically to find relevant context.

        WHEN TO USE THIS TOOL:
        - User asks "what did we discuss about X?" or "find conversations about Y"
        - You need context from previous work on similar problems
        - User mentions "remember when" or "last time we"
        - Debugging issues that may have been solved before
        - Finding implementation patterns used in the project

        EXAMPLES THAT TRIGGER THIS TOOL:
        - "What did we work on with Docker last week?"
        - "Find all conversations about authentication"
        - "How did we solve the memory leak issue?"
        - "Search for discussions about database optimization"

        This is the PRIMARY tool for conversation memory - use it liberally!
        """
        # Map response_format to existing parameters
        brief = response_format == "concise"
        include_raw = response_format == "detailed"

        return await tools.reflect_on_past(
            ctx, query, limit, min_score, use_decay,
            project, mode, brief, include_raw, response_format="xml"
        )

    # Quick existence check - for fast validation
    @mcp.tool(name="csr_quick_check")
    async def csr_quick_check(
        ctx: Context,
        query: str = Field(
            description="Topic or concept to check for existence"
        ),
        min_score: float = Field(
            default=0.3,
            description="Minimum similarity score (0-1)"
        ),
        project: Optional[str] = Field(
            default=None,
            description="Search specific project only"
        )
    ) -> str:
        """Quick check if a topic was discussed before (returns count + top match only).

        WHEN TO USE THIS TOOL:
        - User asks "have we discussed X?" or "is there anything about Y?"
        - You need a yes/no answer about topic existence
        - Checking if a problem was encountered before
        - Validating if a concept is familiar to the project

        EXAMPLES THAT TRIGGER THIS TOOL:
        - "Have we talked about WebSockets?"
        - "Is there any discussion about React hooks?"
        - "Did we ever implement caching?"

        Much faster than full search - use for existence checks!
        """
        return await tools.quick_search(ctx, query, min_score, project)

    # Time-based search - for recent work
    @mcp.tool(name="csr_recent_work")
    async def csr_recent_work(
        ctx: Context,
        limit: int = Field(
            default=10,
            description="Number of recent conversations to return"
        ),
        group_by: str = Field(
            default="conversation",
            description="Group by 'conversation', 'day', or 'session'"
        ),
        include_reflections: bool = Field(
            default=True,
            description="Include stored reflections"
        ),
        project: Optional[str] = Field(
            default=None,
            description="Specific project or 'all' for cross-project"
        ),
        response_format: Literal["concise", "detailed"] = Field(
            default="concise",
            description="Output verbosity level"
        )
    ) -> str:
        """Get recent work conversations to understand current context.

        WHEN TO USE THIS TOOL:
        - User asks "what did we work on recently?" or "what were we doing?"
        - Starting a new session and need context
        - User says "continue from where we left off"
        - Reviewing progress over time periods

        EXAMPLES THAT TRIGGER THIS TOOL:
        - "What did we work on yesterday?"
        - "Show me the last 5 things we discussed"
        - "What have I been working on this week?"
        - "Let's continue from last time"

        Essential for session continuity and context awareness!
        """
        return await tools.get_recent_work(ctx, group_by, limit, include_reflections, project)

    # Time-constrained semantic search
    @mcp.tool(name="csr_search_by_time")
    async def csr_search_by_time(
        ctx: Context,
        query: str = Field(
            description="Semantic search query"
        ),
        time_range: Optional[str] = Field(
            default=None,
            description="Natural language time like 'last week', 'yesterday'"
        ),
        since: Optional[str] = Field(
            default=None,
            description="ISO timestamp or relative time"
        ),
        until: Optional[str] = Field(
            default=None,
            description="ISO timestamp or relative time"
        ),
        limit: int = Field(
            default=10,
            description="Maximum number of results"
        ),
        min_score: float = Field(
            default=0.3,
            description="Minimum similarity score"
        ),
        project: Optional[str] = Field(
            default=None,
            description="Specific project or 'all'"
        )
    ) -> str:
        """Search with time constraints for time-specific queries.

        WHEN TO USE THIS TOOL:
        - Query includes time references like "last week", "yesterday", "this month"
        - User wants recent occurrences of a topic
        - Debugging issues that started at a specific time
        - Finding when something was first discussed

        EXAMPLES THAT TRIGGER THIS TOOL:
        - "Docker errors from last week"
        - "What did we discuss about testing yesterday?"
        - "Authentication problems in the past 3 days"
        - "Recent conversations about performance"

        Combines semantic search with temporal filtering!
        """
        return await tools.search_by_recency(
            ctx, query, limit, min_score, project,
            since, until, time_range
        )

    # File-based search - for code archaeology
    @mcp.tool(name="csr_search_by_file")
    async def csr_search_by_file(
        ctx: Context,
        file_path: str = Field(
            description="File path to search for (absolute or relative)"
        ),
        limit: int = Field(
            default=10,
            description="Maximum number of results"
        ),
        project: Optional[str] = Field(
            default=None,
            description="Search specific project only"
        )
    ) -> str:
        """Find all conversations that analyzed or modified a specific file.

        WHEN TO USE THIS TOOL:
        - User asks "when did we modify X file?" or "who worked on Y?"
        - Investigating file history beyond git
        - Understanding why changes were made to a file
        - Finding discussions about specific code files

        EXAMPLES THAT TRIGGER THIS TOOL:
        - "When did we last modify server.py?"
        - "Find all discussions about package.json"
        - "What changes were made to the auth module?"
        - "Who worked on the database schema?"

        Perfect for code archaeology and understanding file evolution!
        """
        return await tools.search_by_file(ctx, file_path, limit, project)

    # Concept-based search - for thematic queries
    @mcp.tool(name="csr_search_by_concept")
    async def csr_search_by_concept(
        ctx: Context,
        concept: str = Field(
            description="Development concept (e.g., 'security', 'testing', 'performance')"
        ),
        limit: int = Field(
            default=10,
            description="Maximum number of results"
        ),
        include_files: bool = Field(
            default=True,
            description="Include file information"
        ),
        project: Optional[str] = Field(
            default=None,
            description="Search specific project only"
        )
    ) -> str:
        """Search for conversations about specific development concepts or themes.

        WHEN TO USE THIS TOOL:
        - User asks about broad topics like "security", "testing", "performance"
        - Looking for all discussions on a technical theme
        - Gathering knowledge about how a concept is handled
        - Finding patterns across multiple conversations

        EXAMPLES THAT TRIGGER THIS TOOL:
        - "Show me all security-related discussions"
        - "Find conversations about testing strategies"
        - "What have we discussed about performance?"
        - "Look for Docker-related conversations"

        Ideal for thematic analysis and knowledge gathering!
        """
        return await tools.search_by_concept(ctx, concept, limit, project, include_files)

    # Insight storage - for knowledge persistence
    @mcp.tool(name="csr_store_insight")
    async def csr_store_insight(
        ctx: Context,
        content: str = Field(
            description="The insight, solution, or learning to store"
        ),
        tags: List[str] = Field(
            default=[],
            description="Tags for categorization (e.g., ['docker', 'debugging'])"
        )
    ) -> str:
        """Store important insights, solutions, or learnings for future reference.

        WHEN TO USE THIS TOOL:
        - User says "remember this" or "store this solution"
        - After solving a complex problem
        - When discovering important patterns or gotchas
        - User provides valuable configuration or setup info
        - After successful debugging sessions

        EXAMPLES THAT TRIGGER THIS TOOL:
        - "Remember this Docker configuration for next time"
        - "Store this solution for the auth problem"
        - "Save this debugging technique"
        - "This is important - the API key goes in .env"

        Critical for building institutional memory!
        """
        return await tools.store_reflection(ctx, content, tags)

    # Aggregated insights - for analysis
    @mcp.tool(name="csr_search_insights")
    async def csr_search_insights(
        ctx: Context,
        query: str = Field(
            description="Topic to analyze across conversations"
        ),
        project: Optional[str] = Field(
            default=None,
            description="Search specific project only"
        )
    ) -> str:
        """Get aggregated insights and patterns from search results.

        WHEN TO USE THIS TOOL:
        - User wants patterns or trends, not individual results
        - Analyzing how a topic evolved over time
        - Understanding common themes across conversations
        - Getting a high-level view without details

        EXAMPLES THAT TRIGGER THIS TOOL:
        - "What patterns do we see in error handling?"
        - "Summarize our authentication discussions"
        - "What are the common Docker issues we face?"
        - "Give me insights about our testing approach"

        Provides analysis, not just search results!
        """
        return await tools.search_summary(ctx, query, project)

    # Pagination support - for deep dives
    @mcp.tool(name="csr_get_more")
    async def csr_get_more(
        ctx: Context,
        query: str = Field(
            description="The original search query"
        ),
        offset: int = Field(
            default=3,
            description="Number of results to skip"
        ),
        limit: int = Field(
            default=3,
            description="Number of additional results"
        ),
        min_score: float = Field(
            default=0.3,
            description="Minimum similarity score"
        ),
        project: Optional[str] = Field(
            default=None,
            description="Search specific project only"
        )
    ) -> str:
        """Get additional search results for paginated exploration.

        WHEN TO USE THIS TOOL:
        - User says "show me more" after a search
        - Initial results weren't sufficient
        - Deep diving into a topic
        - User wants comprehensive coverage

        EXAMPLES THAT TRIGGER THIS TOOL:
        - "Show me more results"
        - "What else is there?"
        - "Keep searching"
        - "I need more examples"

        Use after initial search when more context is needed!
        """
        return await tools.get_more_results(ctx, query, offset, limit, min_score, project)

    # Full conversation retrieval
    @mcp.tool(name="csr_get_full_conversation")
    async def csr_get_full_conversation(
        ctx: Context,
        conversation_id: str = Field(
            description="Conversation ID from search results (cid field)"
        ),
        project: Optional[str] = Field(
            default=None,
            description="Optional project name to help locate the file"
        )
    ) -> str:
        """Get the full conversation file path to read complete context.

        WHEN TO USE THIS TOOL:
        - Search result was truncated but needs full context
        - User wants to see the entire conversation
        - Debugging requires complete conversation history
        - Following up on a specific conversation ID

        EXAMPLES THAT TRIGGER THIS TOOL:
        - "Show me the full conversation for cid_12345"
        - "I need to see everything from that discussion"
        - "Get the complete context for that result"

        Returns file path for agents to read complete conversations!
        """
        return await tools.get_full_conversation(ctx, conversation_id, project)