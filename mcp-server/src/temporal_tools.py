"""
Temporal tools for Claude Self-Reflect MCP server.
Contains MCP tools for time-based queries and work session tracking.
"""

import os
import logging
import hashlib
from pathlib import Path
from typing import Optional, List, Dict, Any
from datetime import datetime, timezone, timedelta
from collections import Counter, defaultdict

from fastmcp import Context
from pydantic import Field
from qdrant_client import AsyncQdrantClient, models
from qdrant_client.models import OrderBy

from .temporal_utils import SessionDetector, TemporalParser, group_by_time_period
from .project_resolver import ProjectResolver

logger = logging.getLogger(__name__)


class TemporalTools:
    """Temporal query tools for MCP server."""
    
    def __init__(self, 
                 qdrant_client: AsyncQdrantClient,
                 qdrant_url: str,
                 get_all_collections_func,
                 generate_embedding_func,
                 initialize_embeddings_func,
                 normalize_project_name_func):
        """
        Initialize temporal tools.
        
        Args:
            qdrant_client: Async Qdrant client
            qdrant_url: Qdrant server URL
            get_all_collections_func: Function to get all collections
            generate_embedding_func: Function to generate embeddings
            initialize_embeddings_func: Function to initialize embeddings
            normalize_project_name_func: Function to normalize project names
        """
        self.qdrant_client = qdrant_client
        self.qdrant_url = qdrant_url
        self.get_all_collections = get_all_collections_func
        self.generate_embedding = generate_embedding_func
        self.initialize_embeddings = initialize_embeddings_func
        self.normalize_project_name = normalize_project_name_func
    
    async def get_recent_work(
        self,
        ctx: Context,
        limit: int = Field(default=10, description="Number of recent conversations to return"),
        project: Optional[str] = Field(default=None, description="Specific project or 'all' for cross-project"),
        include_reflections: bool = Field(default=True, description="Include stored reflections"),
        group_by: str = Field(default="conversation", description="Group by 'conversation', 'day', or 'session'")
    ) -> str:
        """Get recent work conversations to answer 'What did we work on last?' queries."""
        
        await ctx.debug(f"Getting recent work: limit={limit}, project={project}, group_by={group_by}")
        
        try:
            # Determine project scope
            target_project = project
            if project is None:
                cwd = os.environ.get('MCP_CLIENT_CWD', os.getcwd())
                path_parts = Path(cwd).parts
                if 'projects' in path_parts:
                    idx = path_parts.index('projects')
                    if idx + 1 < len(path_parts):
                        target_project = path_parts[idx + 1]
                if target_project is None:
                    target_project = Path(cwd).name
            
            # Get all collections
            all_collections = await self.get_all_collections()
            if not all_collections:
                return "<error>No conversation collections found. Please import conversations first.</error>"
            
            # Filter collections by project
            if target_project != 'all':
                # Use asyncio.to_thread to avoid blocking the event loop
                import asyncio
                from qdrant_client import QdrantClient as SyncQdrantClient

                def get_project_collections():
                    sync_client = SyncQdrantClient(url=self.qdrant_url)
                    resolver = ProjectResolver(sync_client)
                    return resolver.find_collections_for_project(target_project)

                # Run sync client in thread pool to avoid blocking
                project_collections = await asyncio.to_thread(get_project_collections)
                
                if not project_collections:
                    normalized_name = self.normalize_project_name(target_project)
                    project_hash = hashlib.md5(normalized_name.encode()).hexdigest()[:8]
                    project_collections = [
                        c for c in all_collections 
                        if c.startswith(f"conv_{project_hash}_")
                    ]
                
                if include_reflections:
                    reflections_collections = [c for c in all_collections if c.startswith('reflections')]
                    collections_to_search = list(set(project_collections + reflections_collections))
                else:
                    collections_to_search = project_collections
            else:
                collections_to_search = all_collections
                if not include_reflections:
                    collections_to_search = [c for c in collections_to_search if not c.startswith('reflections')]
            
            await ctx.debug(f"Searching {len(collections_to_search)} collections for recent work")
            
            # Collect recent chunks from all collections
            all_chunks = []
            for collection_name in collections_to_search:
                try:
                    # Use scroll API with native order_by for efficient timestamp sorting
                    results, _ = await self.qdrant_client.scroll(
                        collection_name=collection_name,
                        limit=limit * 2,  # Get more to allow for filtering
                        with_payload=True,
                        order_by=OrderBy(
                            key="timestamp",
                            direction="desc"  # Most recent first
                        )  # Native Qdrant timestamp ordering
                    )
                    
                    for point in results:
                        if point.payload:
                            chunk_data = {
                                'id': str(point.id),
                                'timestamp': point.payload.get('timestamp', datetime.now().isoformat()),
                                'conversation_id': point.payload.get('conversation_id', str(point.id)),
                                'project': point.payload.get('project', target_project),
                                'text': point.payload.get('text', ''),
                                'files_analyzed': point.payload.get('files_analyzed', []),
                                'concepts': point.payload.get('concepts', []),
                                'message_count': point.payload.get('total_messages', 1),
                                'chunk_index': point.payload.get('chunk_index', 0)
                            }
                            
                            # Filter by project if searching all collections but target is specific
                            if target_project != 'all' and not project_collections:
                                # Handle project matching - check if the target project name appears at the end of the stored project path
                                # The stored project name is like "-Users-username-projects-ShopifyMCPMockShop"
                                # We want to match just "ShopifyMCPMockShop"
                                # Also handle underscore/dash variations (procsolve-website vs procsolve_website)
                                point_project = chunk_data['project']
                                normalized_target = target_project.replace('-', '_')
                                normalized_stored = point_project.replace('-', '_')
                                if not (normalized_stored.endswith(f"_{normalized_target}") or 
                                        normalized_stored == normalized_target or
                                        point_project.endswith(f"-{target_project}") or 
                                        point_project == target_project):
                                    continue
                            
                            all_chunks.append(chunk_data)
                            
                except Exception as e:
                    await ctx.debug(f"Error reading {collection_name}: {e}")
                    continue
            
            if not all_chunks:
                return "<no_results>No recent conversations found.</no_results>"
            
            # Sort by timestamp
            parser = TemporalParser()
            all_chunks.sort(key=lambda x: x['timestamp'], reverse=True)
            
            # Apply grouping strategy
            if group_by == "session":
                detector = SessionDetector()
                sessions = detector.detect_sessions(all_chunks[:limit * 3])  # Get more chunks for session detection
                
                result = f"<recent_work sessions='{len(sessions[:limit])}'>\n"
                for session in sessions[:limit]:
                    relative_time = parser.format_relative_time(session.start_time)
                    result += f"  <session time='{relative_time}' duration='{session.duration_minutes}min' project='{session.project}'>\n"
                    result += f"    <topics>{', '.join(session.main_topics[:5])}</topics>\n"
                    if session.files_touched:
                        result += f"    <files>{', '.join(session.files_touched[:5])}</files>\n"
                    result += f"    <stats messages='{session.message_count}' conversations='{len(session.conversation_ids)}'/>\n"
                    result += f"  </session>\n"
                result += "</recent_work>"
                
            elif group_by == "day":
                grouped = group_by_time_period(all_chunks[:limit * 2], granularity='day')
                
                result = f"<recent_work days='{len(grouped)}'>\n"
                for day_key in sorted(grouped.keys(), reverse=True)[:limit]:
                    day_chunks = grouped[day_key]
                    
                    # Aggregate day statistics
                    projects = list(set(c['project'] for c in day_chunks))
                    concepts = []
                    files = []
                    for chunk in day_chunks:
                        concepts.extend(chunk.get('concepts', []))
                        files.extend(chunk.get('files_analyzed', []))
                    
                    # Get most common concepts
                    concept_counts = Counter(concepts)
                    top_concepts = [c for c, _ in concept_counts.most_common(5)]
                    
                    result += f"  <day date='{day_key}' conversations='{len(day_chunks)}'>\n"
                    result += f"    <projects>{', '.join(projects)}</projects>\n"
                    result += f"    <topics>{', '.join(top_concepts)}</topics>\n"
                    if files:
                        unique_files = list(set(files))[:5]
                        result += f"    <files>{', '.join(unique_files)}</files>\n"
                    result += f"  </day>\n"
                result += "</recent_work>"
                
            else:  # Default: group by conversation
                # Group chunks by conversation_id
                conversations = {}
                for chunk in all_chunks:
                    conv_id = chunk.get('conversation_id')
                    if conv_id not in conversations:
                        conversations[conv_id] = []
                    conversations[conv_id].append(chunk)
                
                # Sort conversations by most recent chunk
                sorted_convs = sorted(
                    conversations.items(),
                    key=lambda x: max(c['timestamp'] for c in x[1]),
                    reverse=True
                )
                
                result = f"<recent_work conversations='{min(len(sorted_convs), limit)}'>\n"
                for conv_id, chunks in sorted_convs[:limit]:
                    most_recent = max(chunks, key=lambda x: x['timestamp'])
                    relative_time = parser.format_relative_time(most_recent['timestamp'])
                    
                    # Get conversation summary
                    text_preview = most_recent.get('text', '')[:200]
                    if len(most_recent.get('text', '')) > 200:
                        text_preview += '...'
                    
                    result += f"  <conversation id='{conv_id}' time='{relative_time}' project='{most_recent['project']}'>\n"
                    result += f"    <preview>{text_preview}</preview>\n"
                    
                    # Aggregate concepts and files
                    all_concepts = []
                    all_files = []
                    for chunk in chunks:
                        all_concepts.extend(chunk.get('concepts', []))
                        all_files.extend(chunk.get('files_analyzed', []))
                    
                    if all_concepts:
                        unique_concepts = list(set(all_concepts))[:5]
                        result += f"    <topics>{', '.join(unique_concepts)}</topics>\n"
                    if all_files:
                        unique_files = list(set(all_files))[:3]
                        result += f"    <files>{', '.join(unique_files)}</files>\n"
                    
                    result += f"  </conversation>\n"
                
                result += "</recent_work>"
            
            return result
            
        except Exception as e:
            logger.error(f"Error in get_recent_work: {e}", exc_info=True)
            return f"<error>Failed to get recent work: {str(e)}</error>"
    
    async def search_by_recency(
        self,
        ctx: Context,
        query: str = Field(description="Semantic search query"),
        time_range: Optional[str] = Field(default=None, description="Natural language time like 'last week', 'yesterday'"),
        since: Optional[str] = Field(default=None, description="ISO timestamp or relative time"),
        until: Optional[str] = Field(default=None, description="ISO timestamp or relative time"),
        limit: int = Field(default=10, description="Maximum number of results"),
        min_score: float = Field(default=0.3, description="Minimum similarity score"),
        project: Optional[str] = Field(default=None, description="Specific project or 'all'")
    ) -> str:
        """Time-constrained semantic search for queries like 'docker issues last week'."""
        
        await ctx.debug(f"Search by recency: query='{query}', time_range='{time_range}'")
        
        try:
            # Parse time constraints
            parser = TemporalParser()
            
            if time_range:
                start_time, end_time = parser.parse_time_expression(time_range)
            elif since or until:
                if since:
                    start_time, _ = parser.parse_time_expression(since) if isinstance(since, str) else (since, since)
                else:
                    start_time = datetime.now(timezone.utc) - timedelta(days=30)  # Default 30 days back
                
                if until:
                    _, end_time = parser.parse_time_expression(until) if isinstance(until, str) else (until, until)
                else:
                    end_time = datetime.now(timezone.utc)
            else:
                # Default to last 7 days
                start_time = datetime.now(timezone.utc) - timedelta(days=7)
                end_time = datetime.now(timezone.utc)
            
            await ctx.debug(f"Time range: {start_time.isoformat()} to {end_time.isoformat()}")
            
            # Build filter for Qdrant using DatetimeRange (v1.8.0+)
            time_filter = models.Filter(
                must=[
                    models.FieldCondition(
                        key="timestamp",
                        range=models.DatetimeRange(
                            gte=start_time.isoformat(),
                            lte=end_time.isoformat()
                        )
                    )
                ]
            )
            
            # Get embeddings for query
            if not self.initialize_embeddings():
                return "<error>Failed to initialize embedding models</error>"
            
            # Generate embeddings per collection type
            query_embeddings = {}
            
            # Determine collections to search
            target_project = project
            if project is None:
                cwd = os.environ.get('MCP_CLIENT_CWD', os.getcwd())
                path_parts = Path(cwd).parts
                if 'projects' in path_parts:
                    idx = path_parts.index('projects')
                    if idx + 1 < len(path_parts):
                        target_project = path_parts[idx + 1]
                if target_project is None:
                    target_project = Path(cwd).name
            
            # Get collections
            all_collections = await self.get_all_collections()
            if target_project != 'all':
                from qdrant_client import QdrantClient as SyncQdrantClient
                sync_client = SyncQdrantClient(url=self.qdrant_url)
                resolver = ProjectResolver(sync_client)
                collections_to_search = resolver.find_collections_for_project(target_project)
            else:
                collections_to_search = all_collections
            
            # Search each collection with time filter
            all_results = []
            for collection_name in collections_to_search:
                try:
                    # Determine embedding type for this collection
                    is_local = collection_name.endswith('_local')
                    embedding_type = 'local' if is_local else 'voyage'
                    
                    # Generate or retrieve cached embedding for this type
                    if embedding_type not in query_embeddings:
                        try:
                            query_embeddings[embedding_type] = await self.generate_embedding(query, force_type=embedding_type)
                        except Exception as e:
                            await ctx.debug(f"Failed to generate {embedding_type} embedding: {e}")
                            continue
                    
                    collection_embedding = query_embeddings[embedding_type]
                    
                    results = await self.qdrant_client.search(
                        collection_name=collection_name,
                        query_vector=collection_embedding,
                        query_filter=time_filter,
                        limit=limit,
                        score_threshold=min_score,
                        with_payload=True
                    )
                    
                    for point in results:
                        result_data = {
                            'score': point.score,
                            'timestamp': point.payload.get('timestamp'),
                            'text': point.payload.get('text', ''),
                            'project': point.payload.get('project'),
                            'conversation_id': point.payload.get('conversation_id'),
                            'files_analyzed': point.payload.get('files_analyzed', []),
                            'concepts': point.payload.get('concepts', [])
                        }
                        all_results.append(result_data)
                        
                except Exception as e:
                    await ctx.debug(f"Error searching {collection_name}: {e}")
                    continue
            
            # Sort by score
            all_results.sort(key=lambda x: x['score'], reverse=True)
            all_results = all_results[:limit]
            
            if not all_results:
                return f"<no_results>No results found for '{query}' in the specified time range.</no_results>"
            
            # Format results
            result = f"<search_results query='{query}' time_range='{start_time.date()} to {end_time.date()}' count='{len(all_results)}'>\n"
            
            for idx, res in enumerate(all_results, 1):
                relative_time = parser.format_relative_time(res['timestamp'])
                text_preview = res['text'][:200] + '...' if len(res['text']) > 200 else res['text']
                
                result += f"  <result rank='{idx}' score='{res['score']:.3f}' time='{relative_time}'>\n"
                result += f"    <preview>{text_preview}</preview>\n"
                
                if res.get('concepts'):
                    result += f"    <topics>{', '.join(res['concepts'][:5])}</topics>\n"
                if res.get('files_analyzed'):
                    result += f"    <files>{', '.join(res['files_analyzed'][:3])}</files>\n"
                
                result += f"    <conversation_id>{res['conversation_id']}</conversation_id>\n"
                result += f"  </result>\n"
            
            result += "</search_results>"
            return result
            
        except Exception as e:
            logger.error(f"Error in search_by_recency: {e}", exc_info=True)
            return f"<error>Search failed: {str(e)}</error>"
    
    async def get_timeline(
        self,
        ctx: Context,
        time_range: str = Field(default="last week", description="Natural language time range"),
        project: Optional[str] = Field(default=None, description="Specific project or 'all'"),
        granularity: str = Field(default="day", description="'hour', 'day', 'week', or 'month'"),
        include_stats: bool = Field(default=True, description="Include activity statistics")
    ) -> str:
        """Show activity timeline for a project or across all projects."""
        
        await ctx.debug(f"Getting timeline: time_range='{time_range}', granularity='{granularity}'")
        
        try:
            # Parse time range
            parser = TemporalParser()
            start_time, end_time = parser.parse_time_expression(time_range)
            
            # Determine project scope
            target_project = project
            if project is None:
                cwd = os.environ.get('MCP_CLIENT_CWD', os.getcwd())
                path_parts = Path(cwd).parts
                if 'projects' in path_parts:
                    idx = path_parts.index('projects')
                    if idx + 1 < len(path_parts):
                        target_project = path_parts[idx + 1]
                if target_project is None:
                    target_project = Path(cwd).name
            
            # Get collections
            all_collections = await self.get_all_collections()
            if target_project != 'all':
                from qdrant_client import QdrantClient as SyncQdrantClient
                sync_client = SyncQdrantClient(url=self.qdrant_url)
                resolver = ProjectResolver(sync_client)
                collections_to_search = resolver.find_collections_for_project(target_project)
            else:
                collections_to_search = all_collections
            
            # Build time filter using DatetimeRange (v1.8.0+)
            time_filter = models.Filter(
                must=[
                    models.FieldCondition(
                        key="timestamp",
                        range=models.DatetimeRange(
                            gte=start_time.isoformat(),
                            lte=end_time.isoformat()
                        )
                    )
                ]
            )
            
            # Collect all chunks in time range
            all_chunks = []
            for collection_name in collections_to_search:
                try:
                    # Use scroll with native order_by and time filter for efficient retrieval
                    results, _ = await self.qdrant_client.scroll(
                        collection_name=collection_name,
                        scroll_filter=time_filter,
                        limit=1000,  # Get many items for timeline
                        with_payload=True,
                        order_by=OrderBy(
                            key="timestamp",
                            direction="desc"  # Most recent first
                        )  # Native Qdrant timestamp ordering
                    )
                    
                    for point in results:
                        if point.payload:
                            chunk_data = {
                                'timestamp': point.payload.get('timestamp'),
                                'project': point.payload.get('project', target_project),
                                'conversation_id': point.payload.get('conversation_id'),
                                'files_analyzed': point.payload.get('files_analyzed', []),
                                'files_edited': point.payload.get('files_edited', []),
                                'concepts': point.payload.get('concepts', []),
                                'tools_used': point.payload.get('tools_used', []),
                                'message_count': point.payload.get('total_messages', 1)
                            }
                            all_chunks.append(chunk_data)
                            
                except Exception as e:
                    await ctx.debug(f"Error reading {collection_name}: {e}")
                    continue
            
            if not all_chunks:
                return f"<timeline>No activity found in the specified time range.</timeline>"
            
            # Group by time period
            grouped = group_by_time_period(all_chunks, granularity=granularity)
            
            # Generate timeline
            result = f"<timeline range='{start_time.date()} to {end_time.date()}' periods='{len(grouped)}'>\n"
            
            for period_key in sorted(grouped.keys()):
                period_chunks = grouped[period_key]
                
                result += f"  <period key='{period_key}' conversations='{len(period_chunks)}'>\n"
                
                if include_stats:
                    # Calculate statistics
                    all_files = []
                    all_concepts = []
                    all_tools = []
                    message_count = 0
                    
                    for chunk in period_chunks:
                        all_files.extend(chunk.get('files_analyzed', []))
                        all_files.extend(chunk.get('files_edited', []))
                        all_concepts.extend(chunk.get('concepts', []))
                        all_tools.extend(chunk.get('tools_used', []))
                        message_count += chunk.get('message_count', 1)
                    
                    # Get unique counts and top items
                    file_counts = Counter(all_files)
                    concept_counts = Counter(all_concepts)
                    tool_counts = Counter(all_tools)
                    
                    result += f"    <stats messages='{message_count}' unique_files='{len(file_counts)}' unique_concepts='{len(concept_counts)}'/>\n"
                    
                    if concept_counts:
                        top_concepts = [c for c, _ in concept_counts.most_common(5)]
                        result += f"    <top_topics>{', '.join(top_concepts)}</top_topics>\n"
                    
                    if file_counts:
                        top_files = [f for f, _ in file_counts.most_common(3)]
                        result += f"    <active_files>{', '.join(top_files)}</active_files>\n"
                
                result += f"  </period>\n"
            
            result += "</timeline>"
            return result
            
        except Exception as e:
            logger.error(f"Error in get_timeline: {e}", exc_info=True)
            return f"<error>Failed to generate timeline: {str(e)}</error>"


def register_temporal_tools(
    mcp,
    qdrant_client,
    qdrant_url,
    get_all_collections_func,
    generate_embedding_func,
    initialize_embeddings_func,
    normalize_project_name_func
):
    """
    Register temporal tools with the MCP server.
    
    Args:
        mcp: FastMCP instance
        qdrant_client: Async Qdrant client
        qdrant_url: Qdrant server URL
        get_all_collections_func: Function to get all collections
        generate_embedding_func: Function to generate embeddings
        initialize_embeddings_func: Function to initialize embeddings
        normalize_project_name_func: Function to normalize project names
    
    Returns:
        TemporalTools instance
    """
    # Create temporal tools instance with the provided functions
    tools = TemporalTools(
        qdrant_client=qdrant_client,
        qdrant_url=qdrant_url,
        get_all_collections_func=get_all_collections_func,
        generate_embedding_func=generate_embedding_func,
        initialize_embeddings_func=initialize_embeddings_func,
        normalize_project_name_func=normalize_project_name_func
    )
    
    # Register the tools with MCP
    mcp.tool()(tools.get_recent_work)
    mcp.tool()(tools.search_by_recency)
    mcp.tool()(tools.get_timeline)
    
    return tools