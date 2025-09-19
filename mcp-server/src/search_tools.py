"""Search tools for Claude Self Reflect MCP server."""

import os
import json
import logging
import time
import html
from typing import Optional, List, Dict, Any
from datetime import datetime, timezone
from pathlib import Path

from fastmcp import Context
from pydantic import Field
from qdrant_client import AsyncQdrantClient
from qdrant_client.models import PointStruct

from .parallel_search import parallel_search_collections
from .rich_formatting import format_search_results_rich

logger = logging.getLogger(__name__)


def is_searchable_collection(name: str) -> bool:
    """
    Check if collection name matches searchable patterns.
    Supports both v3 and v4 collection naming conventions.
    """
    return (
        # v3 patterns
        name.endswith('_local')
        or name.endswith('_voyage')
        # v4 patterns
        or name.endswith('_384d')  # Local v4 collections
        or name.endswith('_1024d')  # Cloud v4 collections
        or '_cloud_' in name  # Cloud v4 intermediate naming
        # Reflections
        or name.startswith('reflections')
        # CSR prefixed collections
        or name.startswith('csr_')
    )


class SearchTools:
    """Handles all search operations for the MCP server."""
    
    def __init__(
        self,
        qdrant_client: AsyncQdrantClient,
        qdrant_url: str,
        get_embedding_manager,
        normalize_project_name,
        enable_memory_decay: bool,
        decay_weight: float,
        decay_scale_days: float,
        use_native_decay: bool,
        native_decay_available: bool,
        decay_manager=None,
        project_resolver=None  # Add project resolver
    ):
        """Initialize search tools with dependencies."""
        self.qdrant_client = qdrant_client
        self.qdrant_url = qdrant_url
        self.get_embedding_manager = get_embedding_manager
        self.normalize_project_name = normalize_project_name
        self.enable_memory_decay = enable_memory_decay
        self.decay_weight = decay_weight
        self.decay_scale_days = decay_scale_days
        self.use_native_decay = use_native_decay
        self.native_decay_available = native_decay_available
        self.decay_manager = decay_manager
        self.project_resolver = project_resolver
        
        # Helper functions will be implemented as methods

    def get_project_from_cwd(self, cwd: str) -> Optional[str]:
        """Extract project name from current working directory."""
        from pathlib import Path
        
        path_parts = Path(cwd).parts
        if 'projects' in path_parts:
            idx = path_parts.index('projects')
            if idx + 1 < len(path_parts):
                return path_parts[idx + 1]
        elif '.claude' in path_parts:
            # If we're in a .claude directory, go up to find project
            for i, part in enumerate(path_parts):
                if part == '.claude' and i > 0:
                    return path_parts[i - 1]
        
        # If still no project detected, use the last directory name
        return Path(cwd).name

    async def perform_search(
        self,
        ctx: Context,
        query: str,
        collection_name: str,
        limit: int,
        min_score: float
    ) -> List[Dict[str, Any]]:
        """Perform semantic search on a single collection."""
        try:
            # Generate embedding for query
            embedding_manager = self.get_embedding_manager()
            
            # Determine embedding type based on collection name (v3 and v4 compatible)
            # v4 format: csr_project_mode_dims (e.g., csr_project_cloud_1024d)
            # v3 format: project_suffix (e.g., project_voyage)
            if '_cloud_' in collection_name or collection_name.endswith('_1024d') or collection_name.endswith('_voyage'):
                embedding_type = 'voyage'
            else:
                embedding_type = 'local'
            query_embedding = await embedding_manager.generate_embedding(query, force_type=embedding_type)

            # FIX: Validate embedding before search
            if query_embedding is None:
                logger.warning(f"Embedding generation failed for query in {collection_name}")
                return []

            # Search the collection
            search_results = await self.qdrant_client.search(
                collection_name=collection_name,
                query_vector=query_embedding,
                limit=limit,
                score_threshold=min_score,
                with_payload=True  # Explicitly request payloads from Qdrant
            )

            # CRITICAL FIX: Handle None search results (cloud mode issue)
            if search_results is None:
                logger.warning(f"Search returned None for collection {collection_name}")
                search_results = []

            # Convert results to dict format
            results = []
            for result in search_results:
                # Guard against None payload
                if result.payload is None:
                    logger.warning(f"Result in {collection_name} has None payload, skipping")
                    continue

                results.append({
                    'conversation_id': result.payload.get('conversation_id'),
                    'timestamp': result.payload.get('timestamp'),
                    'content': result.payload.get('content', ''),
                    'score': result.score,
                    'collection': collection_name,
                    'payload': result.payload
                })
            
            return results
            
        except Exception as e:
            await ctx.debug(f"Error searching {collection_name}: {e}")
            return []

    def apply_decay_to_results(self, results: List[Dict], current_time: datetime) -> List[Dict]:
        """Apply time-based decay to search results."""
        if not self.enable_memory_decay:
            return results
        
        for result in results:
            try:
                # Parse timestamp
                timestamp_str = result.get('timestamp')
                if timestamp_str:
                    from datetime import datetime, timezone
                    timestamp = datetime.fromisoformat(timestamp_str.replace('Z', '+00:00'))
                    
                    # Calculate age in days
                    age = (current_time - timestamp).total_seconds() / 86400
                    
                    # Apply exponential decay
                    decay_factor = pow(2, -age / self.decay_scale_days)
                    
                    # Adjust score - FIX: Maintain comparable scale
                    original_score = result['score']
                    result['score'] = original_score * ((1 - self.decay_weight) + self.decay_weight * decay_factor)
                    result['original_score'] = original_score
                    result['decay_factor'] = decay_factor
                    
            except Exception as e:
                logger.warning(f"Error applying decay to result: {e}")
        
        return results

    def format_search_results(
        self,
        results: List[Dict],
        query: str,
        brief: bool = False,
        include_raw: bool = False,
        response_format: str = "xml"
    ) -> str:
        """Format search results for display."""
        if not results:
            return "<search_results><message>No matching conversations found</message></search_results>"
        
        if response_format == "markdown":
            output = f"# Search Results for: {query}\n\n"
            for i, result in enumerate(results, 1):
                output += f"## Result {i}\n"
                output += f"**Score:** {result['score']:.3f}\n"
                output += f"**Timestamp:** {result.get('timestamp', 'N/A')}\n"
                output += f"**Conversation ID:** {result.get('conversation_id', 'N/A')}\n\n"
                if not brief:
                    # Handle both 'content' and 'excerpt' fields
                    content = result.get('content', result.get('excerpt', ''))
                    output += f"**Content:**\n```\n{content[:500]}{'...' if len(content) > 500 else ''}\n```\n\n"
                if include_raw:
                    output += f"**Raw Payload:**\n```json\n{json.dumps(result.get('payload', {}), indent=2)}\n```\n\n"
        else:
            # XML format (default) with proper escaping
            def _esc(x): return html.escape(str(x), quote=False)

            output = f"<search_results>\n<query>{_esc(query)}</query>\n<count>{len(results)}</count>\n"
            for i, result in enumerate(results, 1):
                output += f"<result index=\"{i}\">\n"
                output += f"  <score>{result['score']:.3f}</score>\n"
                output += f"  <timestamp>{_esc(result.get('timestamp', 'N/A'))}</timestamp>\n"
                output += f"  <conversation_id>{_esc(result.get('conversation_id', 'N/A'))}</conversation_id>\n"
                if not brief:
                    # Handle both 'content' and 'excerpt' fields
                    content = result.get('content', result.get('excerpt', result.get('text', '')))
                    truncated = content[:500] + ('...' if len(content) > 500 else '')
                    output += f"  <content><![CDATA[{truncated}]]></content>\n"
                if include_raw:
                    # Use CDATA for large JSON payloads
                    output += f"  <raw_payload><![CDATA[{json.dumps(result.get('payload', {}), ensure_ascii=False)}]]></raw_payload>\n"
                output += "</result>\n"
            output += "</search_results>"
        
        return output

    async def reflect_on_past(
        self,
        ctx: Context,
        query: str,
        limit: int = 5,
        min_score: float = 0.3,
        use_decay: int = -1,
        project: Optional[str] = None,
        mode: str = "full",
        brief: bool = False,
        include_raw: bool = False,
        response_format: str = "xml"
    ) -> str:
        """Search for relevant past conversations using semantic search with optional time decay."""

        await ctx.debug(f"Searching for: {query}, project={project}, mode={mode}, decay={use_decay}")

        try:
            # Track timing for performance metrics
            start_time = time.time()
            timing_info = {}

            # Determine project scope
            target_project = project
            if project is None:
                cwd = os.environ.get('MCP_CLIENT_CWD', os.getcwd())
                target_project = self.get_project_from_cwd(cwd)
                await ctx.debug(f"Inferred project from CWD: {target_project}")
            
            # Handle special cases
            if mode == "quick":
                return await self.quick_search(ctx, query, min_score, target_project)
            elif mode == "summary":
                return await self.search_summary(ctx, query, target_project)
            
            # Get relevant collections based on project
            await ctx.debug(f"Project resolver: {self.project_resolver is not None}, Target project: '{target_project}'")
            if self.project_resolver and target_project and target_project != 'all':
                # Use ProjectResolver to find matching collections
                collection_names = self.project_resolver.find_collections_for_project(target_project)
                await ctx.debug(f"ProjectResolver found {len(collection_names)} collections for '{target_project}'")
                
                # Get collection objects
                collections_response = await self.qdrant_client.get_collections()
                all_collections = collections_response.collections
                filtered_collections = [
                    c for c in all_collections 
                    if c.name in collection_names
                ]
                await ctx.debug(f"Filtered to {len(filtered_collections)} collections from {len(all_collections)} total")
            else:
                # Use all collections INCLUDING reflections (with decay)
                collections_response = await self.qdrant_client.get_collections()

                # Handle None response from Qdrant
                if collections_response is None or not hasattr(collections_response, 'collections'):
                    await ctx.debug(f"WARNING: Qdrant returned None or invalid response")
                    return "<search_results><message>Unable to retrieve collections from Qdrant</message></search_results>"

                collections = collections_response.collections

                # Ensure collections is not None
                if collections is None:
                    await ctx.debug(f"WARNING: collections is None!")
                    return "<search_results><message>No collections available</message></search_results>"

                # Include both conversation collections and reflection collections
                # Use module-level function for consistency
                filtered_collections = [
                    c for c in collections
                    if is_searchable_collection(c.name)
                ]
                await ctx.debug(f"Searching across {len(filtered_collections)} collections")
            
            if not filtered_collections:
                return "<search_results><message>No collections found for the specified project</message></search_results>"

            # Perform PARALLEL search across collections to avoid freeze
            # Ensure filtered_collections is not None before iterating
            if filtered_collections is None:
                await ctx.debug(f"WARNING: filtered_collections is None!")
                return "<search_results><message>No collections available for search</message></search_results>"

            collection_names = [c.name for c in filtered_collections]
            await ctx.debug(f"Starting parallel search across {len(collection_names)} collections")
            
            # Create embedding function wrapper for parallel search
            embedding_manager = self.get_embedding_manager()
            async def generate_embedding_func(text: str, force_type: str = 'local'):
                return await embedding_manager.generate_embedding(text, force_type=force_type)
            
            # Track embedding generation timing
            timing_info['embedding_start'] = time.time()

            # Use parallel search to avoid sequential processing freeze
            all_results, search_timing = await parallel_search_collections(
                collections_to_search=collection_names,
                query=query,
                qdrant_client=self.qdrant_client,
                ctx=ctx,
                limit=limit * 2,  # Get more results initially
                min_score=min_score,
                should_use_decay=use_decay == 1,
                target_project=target_project,
                generate_embedding_func=generate_embedding_func,
                constants={'DECAY_SCALE_DAYS': self.decay_scale_days},
                max_concurrent=10  # Limit concurrent searches to avoid overload
            )

            # Update timing info with search timing
            timing_info['embedding_end'] = time.time()  # Embeddings are generated inside parallel_search
            timing_info['search_all_start'] = timing_info.get('embedding_start', time.time())
            timing_info['search_all_end'] = time.time()
            # search_timing is a list of collection timings, not a dict

            await ctx.debug(f"Parallel search completed with {len(all_results)} total results")

            # Debug: Log some details about results
            if all_results:
                await ctx.debug(f"Top result score: {all_results[0]['score']:.4f}")
            else:
                await ctx.debug(f"No results found. Timing info: {timing_info}")

            if not all_results:
                return "<search_results><message>No matching conversations found</message></search_results>"
            
            # Sort and limit results
            all_results.sort(key=lambda x: x['score'], reverse=True)
            final_results = all_results[:limit]

            # Use rich formatting for default XML format
            if response_format == "xml" and not brief:
                # Try to get indexing status for rich display
                indexing_status = None
                # TODO: Add indexing status retrieval here if needed

                return format_search_results_rich(
                    results=final_results,
                    query=query,
                    target_project=target_project,
                    collections_searched=len(collection_names),
                    timing_info=timing_info,
                    start_time=start_time,
                    brief=brief,
                    include_raw=include_raw,
                    indexing_status=indexing_status
                )
            else:
                # Fall back to standard formatting for markdown or brief mode
                return self.format_search_results(
                    final_results,
                    query,
                    brief=brief,
                    include_raw=include_raw,
                    response_format=response_format
                )
            
        except Exception as e:
            logger.error(f"Search failed: {e}", exc_info=True)
            return f"<search_results><error>Search failed: {str(e)}</error></search_results>"

    async def quick_search(
        self,
        ctx: Context,
        query: str,
        min_score: float = 0.3,
        project: Optional[str] = None
    ) -> str:
        """Quick search that returns only the count and top result for fast overview."""
        
        await ctx.debug(f"Quick search for: {query}, project={project}")
        
        try:
            # Determine project scope
            target_project = project
            if project is None:
                cwd = os.environ.get('MCP_CLIENT_CWD', os.getcwd())
                target_project = self.get_project_from_cwd(cwd)
            
            # Get collections based on project
            if self.project_resolver and target_project and target_project != 'all':
                # Use ProjectResolver to find matching collections
                collection_names = self.project_resolver.find_collections_for_project(target_project)
                collections_response = await self.qdrant_client.get_collections()
                all_collections = collections_response.collections
                filtered_collections = [
                    c for c in all_collections 
                    if c.name in collection_names
                ]
            else:
                # Use all collections INCLUDING reflections (with decay)
                collections_response = await self.qdrant_client.get_collections()
                collections = collections_response.collections
                # Include both conversation collections and reflection collections
                filtered_collections = [
                    c for c in collections
                    if is_searchable_collection(c.name)
                ]
            
            # Quick PARALLEL count across collections
            collection_names = [c.name for c in filtered_collections]
            
            # Create embedding function wrapper
            embedding_manager = self.get_embedding_manager()
            async def generate_embedding_func(text: str, force_type: str = 'local'):
                return await embedding_manager.generate_embedding(text, force_type=force_type)
            
            # Use parallel search for quick check
            all_results, _ = await parallel_search_collections(
                collections_to_search=collection_names,
                query=query,
                qdrant_client=self.qdrant_client,
                ctx=ctx,
                limit=1,  # Only need top result from each collection
                min_score=min_score,
                should_use_decay=False,  # Quick search doesn't use decay
                target_project=target_project,
                generate_embedding_func=generate_embedding_func,
                constants={'DECAY_SCALE_DAYS': self.decay_scale_days},
                max_concurrent=20  # Higher concurrency for quick search
            )
            
            # Count collections with results and find top result
            collections_with_matches = len(set(r.get('collection_name', r.get('collection', '')) for r in all_results))
            top_result = max(all_results, key=lambda x: x.get('score', 0)) if all_results else None
            top_score = top_result.get('score', 0) if top_result else 0
            
            # Format quick search response with proper XML escaping
            def _esc(x): return html.escape(str(x), quote=False)

            if not top_result:
                return "<quick_search><count>0</count><message>No matches found</message></quick_search>"

            # Get preview text and ensure we have content fallbacks
            preview_text = top_result.get('excerpt', top_result.get('content', top_result.get('text', '')))[:200]

            return f"""<quick_search>
<count>{collections_with_matches}</count>
<collections_with_matches>{collections_with_matches}</collections_with_matches>
<top_result>
  <score>{top_result['score']:.3f}</score>
  <timestamp>{_esc(top_result.get('timestamp', 'N/A'))}</timestamp>
  <preview><![CDATA[{preview_text}...]]></preview>
</top_result>
</quick_search>"""

        except Exception as e:
            logger.error(f"Quick search failed: {e}", exc_info=True)
            def _esc(x): return html.escape(str(x), quote=False)
            return f"<quick_search><error>Quick search failed: {_esc(str(e))}</error></quick_search>"

    async def search_summary(
        self,
        ctx: Context,
        query: str,
        project: Optional[str] = None
    ) -> str:
        """Get aggregated insights from search results without individual result details."""
        
        await ctx.debug(f"Getting search summary for: {query}, project={project}")
        
        try:
            # Determine project scope
            target_project = project
            if project is None:
                cwd = os.environ.get('MCP_CLIENT_CWD', os.getcwd())
                target_project = self.get_project_from_cwd(cwd)
            
            # Get collections based on project
            if self.project_resolver and target_project and target_project != 'all':
                # Use ProjectResolver to find matching collections
                collection_names = self.project_resolver.find_collections_for_project(target_project)
                collections_response = await self.qdrant_client.get_collections()
                all_collections = collections_response.collections
                filtered_collections = [
                    c for c in all_collections 
                    if c.name in collection_names
                ]
            else:
                # Use all collections INCLUDING reflections (with decay)
                collections_response = await self.qdrant_client.get_collections()
                collections = collections_response.collections
                # Include both conversation collections and reflection collections
                filtered_collections = [
                    c for c in collections
                    if is_searchable_collection(c.name)
                ]
            
            # Gather results for summary using PARALLEL search
            collection_names = [c.name for c in filtered_collections]
            
            # Create embedding function wrapper
            embedding_manager = self.get_embedding_manager()
            async def generate_embedding_func(text: str, force_type: str = 'local'):
                return await embedding_manager.generate_embedding(text, force_type=force_type)
            
            # Use parallel search for summary
            all_results, _ = await parallel_search_collections(
                collections_to_search=collection_names,
                query=query,
                qdrant_client=self.qdrant_client,
                ctx=ctx,
                limit=10,  # Get more results for summary
                min_score=0.0,  # Get all results for aggregation
                should_use_decay=False,  # Summary doesn't use decay
                target_project=target_project,
                generate_embedding_func=generate_embedding_func,
                constants={'DECAY_SCALE_DAYS': self.decay_scale_days},
                max_concurrent=15  # Balanced concurrency
            )
            
            if not all_results:
                return "<search_summary><message>No matches found</message></search_summary>"
            
            # Sort and get top results
            all_results.sort(key=lambda x: x['score'], reverse=True)
            top_results = all_results[:10]
            
            # Analyze patterns
            avg_score = sum(r['score'] for r in top_results) / len(top_results)
            collections_found = len(set(r.get('collection_name', r.get('collection', '')) for r in top_results))
            
            # Extract common concepts if available
            all_concepts = []
            for r in top_results:
                if 'payload' in r and 'concepts' in r['payload']:
                    all_concepts.extend(r['payload']['concepts'])
            
            from collections import Counter
            concept_counts = Counter(all_concepts).most_common(5)
            
            return f"""<search_summary>
<query>{query}</query>
<total_matches>{len(all_results)}</total_matches>
<average_score>{avg_score:.3f}</average_score>
<collections_matched>{collections_found}</collections_matched>
<top_concepts>{', '.join([c[0] for c in concept_counts]) if concept_counts else 'N/A'}</top_concepts>
<insight>Found {len(all_results)} matches across {collections_found} collections with average relevance of {avg_score:.3f}</insight>
</search_summary>"""
            
        except Exception as e:
            logger.error(f"Search summary failed: {e}", exc_info=True)
            return f"<search_summary><error>Search summary failed: {str(e)}</error></search_summary>"

    async def get_more_results(
        self,
        ctx: Context,
        query: str,
        offset: int = 3,
        limit: int = 3,
        min_score: float = 0.3,
        project: Optional[str] = None
    ) -> str:
        """Get additional search results after an initial search (pagination support)."""
        
        await ctx.debug(f"Getting more results for: {query}, offset={offset}, limit={limit}")
        
        try:
            # Determine project scope
            target_project = project
            if project is None:
                cwd = os.environ.get('MCP_CLIENT_CWD', os.getcwd())
                target_project = self.get_project_from_cwd(cwd)
            
            # Get collections based on project
            if self.project_resolver and target_project and target_project != 'all':
                # Use ProjectResolver to find matching collections
                collection_names = self.project_resolver.find_collections_for_project(target_project)
                collections_response = await self.qdrant_client.get_collections()
                all_collections = collections_response.collections
                filtered_collections = [
                    c for c in all_collections 
                    if c.name in collection_names
                ]
            else:
                # Use all collections INCLUDING reflections (with decay)
                collections_response = await self.qdrant_client.get_collections()
                collections = collections_response.collections
                # Include both conversation collections and reflection collections
                filtered_collections = [
                    c for c in collections
                    if is_searchable_collection(c.name)
                ]
            
            # Gather all results using PARALLEL search
            collection_names = [c.name for c in filtered_collections]
            
            # Create embedding function wrapper
            embedding_manager = self.get_embedding_manager()
            async def generate_embedding_func(text: str, force_type: str = 'local'):
                return await embedding_manager.generate_embedding(text, force_type=force_type)
            
            # Use parallel search for pagination
            all_results, _ = await parallel_search_collections(
                collections_to_search=collection_names,
                query=query,
                qdrant_client=self.qdrant_client,
                ctx=ctx,
                limit=offset + limit,  # Get more results than needed to handle offset
                min_score=min_score,
                should_use_decay=False,  # Pagination doesn't use decay
                target_project=target_project,
                generate_embedding_func=generate_embedding_func,
                constants={'DECAY_SCALE_DAYS': self.decay_scale_days},
                max_concurrent=10  # Standard concurrency
            )
            
            if not all_results:
                return "<more_results><message>No more results found</message></more_results>"
            
            # Sort all results by score
            all_results.sort(key=lambda x: x['score'], reverse=True)
            
            # Apply offset and limit
            paginated_results = all_results[offset:offset + limit]
            
            if not paginated_results:
                return f"<more_results><message>No results at offset {offset}</message></more_results>"
            
            # Format paginated results
            output = f"<more_results>\n<query>{query}</query>\n"
            output += f"<offset>{offset}</offset>\n"
            output += f"<limit>{limit}</limit>\n"
            output += f"<total_available>{len(all_results)}</total_available>\n"
            output += f"<results_returned>{len(paginated_results)}</results_returned>\n"
            
            for i, result in enumerate(paginated_results, 1):
                output += f"<result index=\"{offset + i}\">\n"
                output += f"  <score>{result['score']:.3f}</score>\n"
                output += f"  <timestamp>{result.get('timestamp', 'N/A')}</timestamp>\n"
                output += f"  <preview>{result.get('content', '')[:200]}...</preview>\n"
                output += "</result>\n"
            
            output += "</more_results>"
            return output
            
        except Exception as e:
            logger.error(f"Get more results failed: {e}", exc_info=True)
            return f"<more_results><error>Failed to get more results: {str(e)}</error></more_results>"

    async def search_by_file(
        self,
        ctx: Context,
        file_path: str,
        limit: int = 10,
        project: Optional[str] = None
    ) -> str:
        """Search for conversations that analyzed a specific file."""

        await ctx.debug(f"Searching for file: {file_path}, project={project}")

        try:
            # Create multiple path variants to match how paths are stored
            # Import uses normalize_file_path which replaces /Users/ with ~/
            path_variants = set()

            # Original path
            path_variants.add(file_path)

            # Basename only
            path_variants.add(os.path.basename(file_path))

            # Try to resolve if it's a valid path
            try:
                resolved_path = str(Path(file_path).resolve())
                path_variants.add(resolved_path)

                # Convert resolved path to ~/ format (matching how import stores it)
                home_dir = str(Path.home())
                if resolved_path.startswith(home_dir):
                    tilde_path = resolved_path.replace(home_dir, '~', 1)
                    path_variants.add(tilde_path)

                # Also try with /Users/ replaced by ~/
                if '/Users/' in resolved_path:
                    path_variants.add(resolved_path.replace('/Users/', '~/', 1))
            except:
                pass

            # If path starts with ~, also try expanded version
            if file_path.startswith('~'):
                expanded = os.path.expanduser(file_path)
                path_variants.add(expanded)

            # Convert all to forward slashes for consistency
            path_variants = {p.replace('\\', '/') for p in path_variants if p}

            await ctx.debug(f"Searching with path variants: {list(path_variants)}")

            # Search for file mentions in metadata
            collections_response = await self.qdrant_client.get_collections()
            collections = collections_response.collections

            # Define async function to search a single collection using scroll
            async def search_collection(collection_name: str):
                try:
                    from qdrant_client import models

                    # Use scroll with proper filter for metadata-only search
                    results, _ = await self.qdrant_client.scroll(
                        collection_name=collection_name,
                        scroll_filter=models.Filter(
                            should=[
                                models.FieldCondition(
                                    key="files_analyzed",
                                    match=models.MatchValue(value=path_variant)
                                )
                                for path_variant in path_variants
                            ]
                        ),
                        limit=limit,
                        with_payload=True
                    )

                    formatted_results = []
                    for point in results:
                        formatted_results.append({
                            'conversation_id': point.payload.get('conversation_id'),
                            'timestamp': point.payload.get('timestamp'),
                            'content': point.payload.get('content', point.payload.get('text', '')),
                            'files_analyzed': point.payload.get('files_analyzed', []),
                            'score': 1.0  # No score in scroll, use 1.0 for found items
                        })
                    return formatted_results

                except Exception as e:
                    await ctx.debug(f"Error searching {collection_name}: {e}")
                    return []
            
            # SECURITY FIX: Use proper concurrency limiting
            import asyncio
            from .security_patches import ConcurrencyLimiter

            search_tasks = [search_collection(c.name) for c in collections]

            # Use semaphore-based limiting instead of batching
            all_results = []
            batch_results = await ConcurrencyLimiter.limited_gather(search_tasks, limit=10)
            for results in batch_results:
                if isinstance(results, Exception):
                    logger.error(f"Search task failed: {type(results).__name__}: {results}")
                    await ctx.debug(f"Search task error: {results}")
                    continue
                if results:
                    all_results.extend(results)
            
            # Format results
            if not all_results:
                return f"<file_search><message>No conversations found analyzing {file_path}</message></file_search>"
            
            return self.format_search_results(all_results, f"file:{file_path}")
            
        except Exception as e:
            logger.error(f"File search failed: {e}", exc_info=True)
            return f"<file_search><error>File search failed: {str(e)}</error></file_search>"

    async def search_by_concept(
        self,
        ctx: Context,
        concept: str,
        limit: int = 10,
        project: Optional[str] = None,
        include_files: bool = True
    ) -> str:
        """Search for conversations about a specific development concept."""
        
        await ctx.debug(f"Searching for concept: {concept}, project={project}")
        
        try:
            # Search using concept as query with semantic search
            results = await self.reflect_on_past(
                ctx, concept, limit=limit, project=project
            )
            
            # Enhance results with concept-specific formatting
            # This is a simplified version - actual implementation would analyze concepts
            return results
            
        except Exception as e:
            logger.error(f"Concept search failed: {e}", exc_info=True)
            return f"<concept_search><error>Concept search failed: {str(e)}</error></concept_search>"

    async def get_next_results(
        self,
        ctx: Context,
        query: str,
        offset: int = 3,
        limit: int = 3,
        min_score: float = 0.3,
        project: Optional[str] = None
    ) -> str:
        """Get additional search results after an initial search (pagination support)."""
        # This is an alias for get_more_results
        return await self.get_more_results(ctx, query, offset, limit, min_score, project)


def register_search_tools(
    mcp,
    qdrant_client: AsyncQdrantClient,
    qdrant_url: str,
    get_embedding_manager,
    normalize_project_name,
    enable_memory_decay: bool,
    decay_weight: float,
    decay_scale_days: float,
    use_native_decay: bool,
    native_decay_available: bool,
    decay_manager=None,
    project_resolver=None  # Add project resolver
):
    """Register search tools with the MCP server."""
    
    tools = SearchTools(
        qdrant_client,
        qdrant_url,
        get_embedding_manager,
        normalize_project_name,
        enable_memory_decay,
        decay_weight,
        decay_scale_days,
        use_native_decay,
        native_decay_available,
        decay_manager,
        project_resolver  # Pass the resolver
    )
    
    @mcp.tool(name="csr_reflect_on_past")
    async def reflect_on_past(
        ctx: Context,
        query: str = Field(description="The search query to find semantically similar conversations"),
        limit: int = Field(default=5, description="Maximum number of results to return"),
        min_score: float = Field(default=0.3, description="Minimum similarity score (0-1)"),
        use_decay: int = Field(default=-1, description="Apply time-based decay: 1=enable, 0=disable, -1=use environment default (accepts int or str)"),
        project: Optional[str] = Field(default=None, description="Search specific project only. If not provided, searches current project based on working directory. Use 'all' to search across all projects."),
        mode: str = Field(default="full", description="Search mode: 'full' (all results with details), 'quick' (count + top result only), 'summary' (aggregated insights without individual results)"),
        brief: bool = Field(default=False, description="Brief mode: returns minimal information for faster response"),
        include_raw: bool = Field(default=False, description="Include raw Qdrant payload data for debugging (increases response size)"),
        response_format: str = Field(default="xml", description="Response format: 'xml' or 'markdown'")
    ) -> str:
        """Search past Claude conversations semantically to find relevant context.

        WHEN TO USE: User asks 'what did we discuss about X?', 'find conversations about Y',
        mentions 'remember when' or 'last time', debugging issues that may have been solved before,
        or finding implementation patterns used in the project.

        This is the PRIMARY tool for conversation memory - use it liberally!"""
        return await tools.reflect_on_past(ctx, query, limit, min_score, use_decay, project, mode, brief, include_raw, response_format)
    
    @mcp.tool(name="csr_quick_check")
    async def quick_search(
        ctx: Context,
        query: str = Field(description="The search query to find semantically similar conversations"),
        min_score: float = Field(default=0.3, description="Minimum similarity score (0-1)"),
        project: Optional[str] = Field(default=None, description="Search specific project only. If not provided, searches current project based on working directory. Use 'all' to search across all projects.")
    ) -> str:
        """Quick check if a topic was discussed before (returns count + top match only).

        WHEN TO USE: User asks 'have we discussed X?' or 'is there anything about Y?',
        need a yes/no answer about topic existence, checking if a problem was encountered before.

        Much faster than full search - use for existence checks!"""
        return await tools.quick_search(ctx, query, min_score, project)
    
    @mcp.tool(name="csr_search_insights")
    async def search_summary(
        ctx: Context,
        query: str = Field(description="The search query to find semantically similar conversations"),
        project: Optional[str] = Field(default=None, description="Search specific project only. If not provided, searches current project based on working directory. Use 'all' to search across all projects.")
    ) -> str:
        """Get aggregated insights and patterns from search results.

        WHEN TO USE: User wants patterns or trends, analyzing topic evolution,
        understanding common themes, getting high-level view without details.

        Provides analysis, not just search results!"""
        return await tools.search_summary(ctx, query, project)
    
    @mcp.tool(name="csr_get_more")
    async def get_more_results(
        ctx: Context,
        query: str = Field(description="The original search query"),
        offset: int = Field(default=3, description="Number of results to skip (for pagination)"),
        limit: int = Field(default=3, description="Number of additional results to return"),
        min_score: float = Field(default=0.3, description="Minimum similarity score (0-1)"),
        project: Optional[str] = Field(default=None, description="Search specific project only")
    ) -> str:
        """Get additional search results for paginated exploration.

        WHEN TO USE: User says 'show me more' after a search, initial results weren't sufficient,
        deep diving into a topic, user wants comprehensive coverage.

        Use after initial search when more context is needed!"""
        return await tools.get_more_results(ctx, query, offset, limit, min_score, project)
    
    @mcp.tool(name="csr_search_by_file")
    async def search_by_file(
        ctx: Context,
        file_path: str = Field(description="The file path to search for in conversations"),
        limit: int = Field(default=10, description="Maximum number of results to return"),
        project: Optional[str] = Field(default=None, description="Search specific project only. Use 'all' to search across all projects.")
    ) -> str:
        """Find all conversations that analyzed or modified a specific file.

        WHEN TO USE: User asks 'when did we modify X file?', investigating file history,
        understanding why changes were made, finding discussions about specific code files.

        Perfect for code archaeology and understanding file evolution!"""
        return await tools.search_by_file(ctx, file_path, limit, project)
    
    @mcp.tool(name="csr_search_by_concept")
    async def search_by_concept(
        ctx: Context,
        concept: str = Field(description="The concept to search for (e.g., 'security', 'docker', 'testing')"),
        limit: int = Field(default=10, description="Maximum number of results to return"),
        project: Optional[str] = Field(default=None, description="Search specific project only. Use 'all' to search across all projects."),
        include_files: bool = Field(default=True, description="Include file information in results")
    ) -> str:
        """Search for conversations about specific development concepts or themes.

        WHEN TO USE: User asks about broad topics like 'security', 'testing', 'performance',
        looking for all discussions on a technical theme, gathering knowledge about a concept.

        Ideal for thematic analysis and knowledge gathering!"""
        return await tools.search_by_concept(ctx, concept, limit, project, include_files)
    
    @mcp.tool()
    async def get_next_results(
        ctx: Context,
        query: str = Field(description="The original search query"),
        offset: int = Field(default=3, description="Number of results to skip (for pagination)"),
        limit: int = Field(default=3, description="Number of additional results to return"),
        min_score: float = Field(default=0.3, description="Minimum similarity score (0-1)"),
        project: Optional[str] = Field(default=None, description="Search specific project only")
    ) -> str:
        """Get additional search results after an initial search (pagination support)."""
        return await tools.get_next_results(ctx, query, offset, limit, min_score, project)
    
    logger.info("Search tools registered successfully")