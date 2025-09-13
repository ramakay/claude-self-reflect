"""
Parallel search implementation for Qdrant collections.
This module implements asyncio.gather-based parallel searching to improve performance.
"""

import asyncio
import time
from typing import List, Dict, Any, Optional, Tuple
from datetime import datetime
import logging

logger = logging.getLogger(__name__)

async def search_single_collection(
    collection_name: str,
    query: str,
    query_embeddings: Dict[str, List[float]],
    qdrant_client: Any,
    ctx: Any,
    limit: int,
    min_score: float,
    should_use_decay: bool,
    target_project: str,
    generate_embedding_func: Any,
    constants: Dict[str, Any]
) -> Tuple[str, List[Any], Dict[str, Any]]:
    """
    Search a single collection and return results.
    
    Returns:
        Tuple of (collection_name, results, timing_info)
    """
    collection_timing = {'name': collection_name, 'start': time.time()}
    results = []
    
    try:
        # Determine embedding type for this collection
        embedding_type_for_collection = 'voyage' if collection_name.endswith('_voyage') else 'local'
        logger.debug(f"Collection {collection_name} needs {embedding_type_for_collection} embedding")

        # Generate or retrieve cached embedding for this type
        if embedding_type_for_collection not in query_embeddings:
            try:
                query_embeddings[embedding_type_for_collection] = await generate_embedding_func(
                    query, force_type=embedding_type_for_collection
                )
            except Exception as e:
                await ctx.debug(f"Failed to generate {embedding_type_for_collection} embedding: {e}")
                collection_timing['error'] = str(e)
                collection_timing['end'] = time.time()
                return collection_name, results, collection_timing
        
        query_embedding = query_embeddings[embedding_type_for_collection]
        
        # Check if this is a reflections collection
        is_reflection_collection = collection_name == 'reflections'
        
        # Import necessary models
        from qdrant_client import models
        
        # Determine which decay method to use
        USE_NATIVE_DECAY = constants.get('USE_NATIVE_DECAY', False)
        NATIVE_DECAY_AVAILABLE = constants.get('NATIVE_DECAY_AVAILABLE', False)
        DECAY_SCALE_DAYS = constants.get('DECAY_SCALE_DAYS', 90)
        DECAY_WEIGHT = constants.get('DECAY_WEIGHT', 0.3)
        
        if should_use_decay and USE_NATIVE_DECAY and NATIVE_DECAY_AVAILABLE:
            # Use native Qdrant decay with newer API
            await ctx.debug(f"Using NATIVE Qdrant decay (new API) for {collection_name}")
            
            half_life_seconds = DECAY_SCALE_DAYS * 24 * 60 * 60
            
            # Build query using Qdrant's Fusion and RankFusion
            fusion_query = models.Fusion(
                fusion=models.RankFusion.RRF,
                queries=[
                    # Semantic similarity query
                    models.NearestQuery(
                        nearest=query_embedding,
                        score_threshold=min_score
                    ),
                    # Time decay query using context pair
                    models.ContextQuery(
                        context=[
                            models.ContextPair(
                                positive=models.DiscoverQuery(
                                    target=query_embedding,
                                    context=[
                                        models.ContextPair(
                                            positive=models.DatetimeRange(
                                                gt=datetime.now().isoformat(),
                                                lt=(datetime.now().timestamp() + half_life_seconds)
                                            )
                                        )
                                    ]
                                )
                            )
                        ]
                    )
                ]
            )
            
            # Execute search with native decay
            search_results = await qdrant_client.query_points(
                collection_name=collection_name,
                query=fusion_query,
                limit=limit,
                with_payload=True
            )
            
            # Process results
            for point in search_results.points:
                # Process each point and add to results
                raw_timestamp = point.payload.get('timestamp', datetime.now().isoformat())
                clean_timestamp = raw_timestamp.replace('Z', '+00:00') if raw_timestamp.endswith('Z') else raw_timestamp
                
                point_project = point.payload.get('project', collection_name.replace('conv_', '').replace('_voyage', '').replace('_local', ''))
                
                # Apply project filtering
                if target_project != 'all' and not is_reflection_collection:
                    if point_project != target_project:
                        normalized_target = target_project.replace('-', '_')
                        normalized_point = point_project.replace('-', '_')
                        if not (normalized_point == normalized_target or 
                                point_project.endswith(f"/{target_project}") or
                                point_project.endswith(f"-{target_project}") or
                                normalized_point.endswith(f"_{normalized_target}") or
                                normalized_point.endswith(f"/{normalized_target}")):
                            continue
                
                # Create SearchResult
                search_result = {
                    'id': str(point.id),
                    'score': point.score,
                    'timestamp': clean_timestamp,
                    'role': point.payload.get('start_role', point.payload.get('role', 'unknown')),
                    'excerpt': (point.payload.get('text', '')[:350] + '...' 
                               if len(point.payload.get('text', '')) > 350 
                               else point.payload.get('text', '')),
                    'project_name': point_project,
                    'payload': point.payload
                }
                results.append(search_result)
                
        else:
            # Standard search without native decay or client-side decay
            search_results = await qdrant_client.search(
                collection_name=collection_name,
                query_vector=query_embedding,
                limit=limit * 3 if should_use_decay else limit,  # Get more results for client-side decay
                score_threshold=min_score if not should_use_decay else 0.0,
                with_payload=True
            )

            # Debug: Log search results
            logger.debug(f"Search of {collection_name} returned {len(search_results)} results")
            
            if should_use_decay and not USE_NATIVE_DECAY:
                # Apply client-side decay
                await ctx.debug(f"Using CLIENT-SIDE decay for {collection_name}")
                decay_results = []
                
                for point in search_results:
                    try:
                        raw_timestamp = point.payload.get('timestamp', datetime.now().isoformat())
                        clean_timestamp = raw_timestamp.replace('Z', '+00:00') if raw_timestamp.endswith('Z') else raw_timestamp
                        
                        # Calculate age and decay
                        if 'timestamp' in point.payload:
                            try:
                                point_time = datetime.fromisoformat(clean_timestamp)
                                if point_time.tzinfo is None:
                                    from zoneinfo import ZoneInfo
                                    point_time = point_time.replace(tzinfo=ZoneInfo('UTC'))
                                
                                now = datetime.now(ZoneInfo('UTC'))
                                age_ms = (now - point_time).total_seconds() * 1000
                                
                                # Exponential decay with configurable half-life
                                half_life_ms = DECAY_SCALE_DAYS * 24 * 60 * 60 * 1000
                                decay_factor = 0.5 ** (age_ms / half_life_ms)
                                
                                # Apply multiplicative decay
                                adjusted_score = point.score * ((1 - DECAY_WEIGHT) + DECAY_WEIGHT * decay_factor)
                            except Exception as e:
                                await ctx.debug(f"Error calculating decay: {e}")
                                adjusted_score = point.score
                        else:
                            adjusted_score = point.score
                        
                        # Only include if above min_score after decay
                        if adjusted_score >= min_score:
                            decay_results.append((adjusted_score, point))
                    
                    except Exception as e:
                        await ctx.debug(f"Error applying decay to point: {e}")
                        decay_results.append((point.score, point))
                
                # Sort by adjusted score and take top results
                decay_results.sort(key=lambda x: x[0], reverse=True)
                
                # Convert to SearchResult format
                for adjusted_score, point in decay_results[:limit]:
                    raw_timestamp = point.payload.get('timestamp', datetime.now().isoformat())
                    clean_timestamp = raw_timestamp.replace('Z', '+00:00') if raw_timestamp.endswith('Z') else raw_timestamp
                    
                    point_project = point.payload.get('project', collection_name.replace('conv_', '').replace('_voyage', '').replace('_local', ''))
                    
                    # Apply project filtering
                    if target_project != 'all' and not is_reflection_collection:
                        if point_project != target_project:
                            normalized_target = target_project.replace('-', '_')
                            normalized_point = point_project.replace('-', '_')
                            if not (normalized_point == normalized_target or
                                    point_project.endswith(f"/{target_project}") or
                                    point_project.endswith(f"-{target_project}") or
                                    normalized_point.endswith(f"_{normalized_target}") or
                                    normalized_point.endswith(f"/{normalized_target}")):
                                logger.debug(f"Filtering out point: project '{point_project}' != target '{target_project}'")
                                continue
                        logger.debug(f"Keeping point: project '{point_project}' matches target '{target_project}'")
                    
                    # Create SearchResult
                    search_result = {
                        'id': str(point.id),
                        'score': adjusted_score,
                        'timestamp': clean_timestamp,
                        'role': point.payload.get('start_role', point.payload.get('role', 'unknown')),
                        'excerpt': (point.payload.get('text', '')[:350] + '...' 
                                   if len(point.payload.get('text', '')) > 350 
                                   else point.payload.get('text', '')),
                        'project_name': point_project,
                        'payload': point.payload
                    }
                    results.append(search_result)
            else:
                # Process standard search results without decay
                logger.debug(f"Processing {len(search_results)} results from {collection_name}")
                for point in search_results:
                    raw_timestamp = point.payload.get('timestamp', datetime.now().isoformat())
                    clean_timestamp = raw_timestamp.replace('Z', '+00:00') if raw_timestamp.endswith('Z') else raw_timestamp

                    point_project = point.payload.get('project', collection_name.replace('conv_', '').replace('_voyage', '').replace('_local', ''))
                    
                    # Apply project filtering
                    if target_project != 'all' and not is_reflection_collection:
                        if point_project != target_project:
                            normalized_target = target_project.replace('-', '_')
                            normalized_point = point_project.replace('-', '_')
                            if not (normalized_point == normalized_target or
                                    point_project.endswith(f"/{target_project}") or
                                    point_project.endswith(f"-{target_project}") or
                                    normalized_point.endswith(f"_{normalized_target}") or
                                    normalized_point.endswith(f"/{normalized_target}")):
                                logger.debug(f"Filtering out point: project '{point_project}' != target '{target_project}'")
                                continue
                        logger.debug(f"Keeping point: project '{point_project}' matches target '{target_project}'")
                    
                    # Create SearchResult as dictionary (consistent with other branches)
                    search_result = {
                        'id': str(point.id),
                        'score': point.score,
                        'timestamp': clean_timestamp,
                        'role': point.payload.get('start_role', point.payload.get('role', 'unknown')),
                        'excerpt': (point.payload.get('text', '')[:350] + '...'
                                   if len(point.payload.get('text', '')) > 350
                                   else point.payload.get('text', '')),
                        'project_name': point_project,
                        'conversation_id': point.payload.get('conversation_id'),
                        'base_conversation_id': point.payload.get('base_conversation_id'),
                        'collection_name': collection_name,
                        'raw_payload': point.payload,
                        'code_patterns': point.payload.get('code_patterns'),
                        'files_analyzed': point.payload.get('files_analyzed'),
                        'tools_used': list(point.payload.get('tools_used', [])) if isinstance(point.payload.get('tools_used'), set) else point.payload.get('tools_used'),
                        'concepts': point.payload.get('concepts')
                    }
                    results.append(search_result)
    
    except Exception as e:
        await ctx.debug(f"Error searching {collection_name}: {str(e)}")
        collection_timing['error'] = str(e)
    
    collection_timing['end'] = time.time()
    logger.debug(f"Collection {collection_name} returning {len(results)} results after filtering")
    return collection_name, results, collection_timing


async def parallel_search_collections(
    collections_to_search: List[str],
    query: str,
    qdrant_client: Any,
    ctx: Any,
    limit: int,
    min_score: float,
    should_use_decay: bool,
    target_project: str,
    generate_embedding_func: Any,
    constants: Dict[str, Any],
    max_concurrent: int = 10
) -> Tuple[List[Any], List[Dict[str, Any]]]:
    """
    Search multiple collections in parallel using asyncio.gather.
    
    Args:
        collections_to_search: List of collection names to search
        query: Search query
        qdrant_client: Qdrant client instance
        ctx: Context for debugging
        limit: Maximum results per collection
        min_score: Minimum similarity score
        should_use_decay: Whether to apply time decay
        target_project: Project filter ('all' or specific project)
        generate_embedding_func: Function to generate embeddings
        constants: Dictionary of constants (USE_NATIVE_DECAY, etc.)
        max_concurrent: Maximum concurrent searches
    
    Returns:
        Tuple of (all_results, collection_timings)
    """
    await ctx.debug(f"Starting parallel search across {len(collections_to_search)} collections")
    
    # Shared cache for embeddings
    query_embeddings = {}
    
    # Create semaphore to limit concurrent operations
    semaphore = asyncio.Semaphore(max_concurrent)
    
    async def search_with_semaphore(collection_name: str) -> Tuple[str, List[Any], Dict[str, Any]]:
        """Search with concurrency limit"""
        async with semaphore:
            return await search_single_collection(
                collection_name=collection_name,
                query=query,
                query_embeddings=query_embeddings,
                qdrant_client=qdrant_client,
                ctx=ctx,
                limit=limit,
                min_score=min_score,
                should_use_decay=should_use_decay,
                target_project=target_project,
                generate_embedding_func=generate_embedding_func,
                constants=constants
            )
    
    # Launch all searches in parallel
    search_tasks = [
        search_with_semaphore(collection_name)
        for collection_name in collections_to_search
    ]
    
    # Wait for all searches to complete
    search_results = await asyncio.gather(*search_tasks, return_exceptions=True)
    
    # Process results
    all_results = []
    collection_timings = []
    
    for result in search_results:
        if isinstance(result, Exception):
            # Handle exceptions from gather
            logger.error(f"Search task failed: {result}")
            continue
        
        collection_name, results, timing = result
        all_results.extend(results)
        collection_timings.append(timing)
    
    await ctx.debug(f"Parallel search complete: {len(all_results)} total results")
    
    return all_results, collection_timings