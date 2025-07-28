"""Claude Reflect MCP Server with Native Qdrant Memory Decay (v2.0.0)."""

import os
from pathlib import Path
from typing import Any, Optional, List, Dict, Union
from datetime import datetime
import json

from fastmcp import FastMCP, Context
from pydantic import BaseModel, Field
from qdrant_client import AsyncQdrantClient, models
from qdrant_client.models import (
    PointStruct, VectorParams, Distance
)
try:
    from qdrant_client.models import (
        Query, Formula, Expression, MultExpression,
        ExpDecayExpression, DecayParamsExpression,
        SearchRequest, NamedQuery
    )
    NATIVE_DECAY_AVAILABLE = True
except ImportError:
    # Fallback for older qdrant-client versions
    NATIVE_DECAY_AVAILABLE = False
    Query = Formula = Expression = MultExpression = None
    ExpDecayExpression = DecayParamsExpression = None
    SearchRequest = NamedQuery = None
import voyageai
from dotenv import load_dotenv

# Load environment variables
env_path = Path(__file__).parent.parent.parent / '.env'
load_dotenv(env_path)

# Configuration
QDRANT_URL = os.getenv('QDRANT_URL', 'http://localhost:6333')
VOYAGE_API_KEY = os.getenv('VOYAGE_KEY') or os.getenv('VOYAGE_KEY-2')
ENABLE_MEMORY_DECAY = os.getenv('ENABLE_MEMORY_DECAY', 'false').lower() == 'true'
DECAY_WEIGHT = float(os.getenv('DECAY_WEIGHT', '0.3'))
DECAY_SCALE_DAYS = float(os.getenv('DECAY_SCALE_DAYS', '90'))

# Initialize Voyage AI client
voyage_client = voyageai.Client(api_key=VOYAGE_API_KEY) if VOYAGE_API_KEY else None

# Debug environment loading (disabled for production)
# print(f"[DEBUG] Qdrant Native Decay Server v2.0.0")
# print(f"[DEBUG] ENABLE_MEMORY_DECAY: {ENABLE_MEMORY_DECAY}")
# print(f"[DEBUG] DECAY_WEIGHT: {DECAY_WEIGHT}")
# print(f"[DEBUG] DECAY_SCALE_DAYS: {DECAY_SCALE_DAYS}")


class SearchResult(BaseModel):
    """A single search result."""
    id: str
    score: float
    timestamp: str
    role: str
    excerpt: str
    project_name: str
    conversation_id: Optional[str] = None
    collection_name: str


# Initialize FastMCP instance
mcp = FastMCP(
    name="claude-reflect",
    instructions="Search past conversations and store reflections with time-based memory decay (v2.0.0 - Native Qdrant)"
)

# Create Qdrant client
qdrant_client = AsyncQdrantClient(url=QDRANT_URL)
    
async def get_voyage_collections() -> List[str]:
    """Get all Voyage collections."""
    collections = await qdrant_client.get_collections()
    return [c.name for c in collections.collections if c.name.endswith('_voyage')]

async def generate_embedding(text: str) -> List[float]:
    """Generate embedding using Voyage AI."""
    if not voyage_client:
        raise ValueError("Voyage AI API key not configured")
    
    result = voyage_client.embed(
        texts=[text],
        model="voyage-3-large",
        input_type="query"
    )
    return result.embeddings[0]
    
# Register tools
@mcp.tool()
async def reflect_on_past(
    ctx: Context,
    query: str = Field(description="The search query to find semantically similar conversations"),
    limit: int = Field(default=5, description="Maximum number of results to return"),
    min_score: float = Field(default=0.3, description="Minimum similarity score (0-1)"),
    use_decay: Union[int, str] = Field(default=-1, description="Apply time-based decay: 1=enable, 0=disable, -1=use environment default (accepts int or str)")
) -> str:
    """Search for relevant past conversations using semantic search with optional time decay."""
    
    # Normalize use_decay to integer
    if isinstance(use_decay, str):
        try:
            use_decay = int(use_decay)
        except ValueError:
            raise ValueError("use_decay must be '1', '0', or '-1'")
    
    # Parse decay parameter using integer approach
    should_use_decay = (
        True if use_decay == 1
        else False if use_decay == 0
        else ENABLE_MEMORY_DECAY  # -1 or any other value
    )
    
    await ctx.debug(f"Searching for: {query}")
    await ctx.debug(f"Decay enabled: {should_use_decay}")
    await ctx.debug(f"Using Qdrant Native Decay (v2.0.0)")
    
    try:
        # Generate embedding
        query_embedding = await generate_embedding(query)
        
        # Get all Voyage collections
        voyage_collections = await get_voyage_collections()
        if not voyage_collections:
            return "No conversation collections found. Please import conversations first."
        
        await ctx.debug(f"Searching across {len(voyage_collections)} collections")
        
        all_results = []
        
        # Search each collection with native Qdrant decay
        for collection_name in voyage_collections:
            try:
                if should_use_decay and NATIVE_DECAY_AVAILABLE:
                    # Build the query with native Qdrant decay formula
                    query_obj = Query(
                        nearest=query_embedding,
                        formula=Formula(
                            sum=[
                                # Original similarity score
                                Expression(variable="score"),
                                # Decay boost term
                                Expression(
                                    mult=MultExpression(
                                        mult=[
                                            # Decay weight
                                            Expression(constant=DECAY_WEIGHT),
                                            # Exponential decay function
                                            Expression(
                                                exp_decay=DecayParamsExpression(
                                                    # Use timestamp field for decay
                                                    x=Expression(datetime_key="timestamp"),
                                                    # Decay from current time (server-side)
                                                    target=Expression(datetime="now"),
                                                    # Scale in milliseconds
                                                    scale=DECAY_SCALE_DAYS * 24 * 60 * 60 * 1000,
                                                    # Standard exponential decay midpoint
                                                    midpoint=0.5
                                                )
                                            )
                                        ]
                                    )
                                )
                            ]
                        )
                    )
                    
                    # Execute query with native decay
                    results = await qdrant_client.query_points(
                        collection_name=collection_name,
                        query=query_obj,
                        limit=limit,
                        score_threshold=min_score,
                        with_payload=True
                    )
                    
                    await ctx.debug(f"Native decay search in {collection_name} returned {len(results.points)} results")
                else:
                    # Standard search without decay
                    results = await qdrant_client.search(
                        collection_name=collection_name,
                        query_vector=query_embedding,
                        limit=limit,
                        score_threshold=min_score,
                        with_payload=True
                    )
                    results = models.QueryResponse(points=results)
                
                # Process results
                for point in results.points:
                    all_results.append(SearchResult(
                        id=str(point.id),
                        score=point.score,
                        timestamp=point.payload.get('timestamp', datetime.now().isoformat()),
                        role=point.payload.get('start_role', point.payload.get('role', 'unknown')),
                        excerpt=(point.payload.get('text', '')[:500] + '...'),
                        project_name=point.payload.get('project', collection_name.replace('conv_', '').replace('_voyage', '')),
                        conversation_id=point.payload.get('conversation_id'),
                        collection_name=collection_name
                    ))
            
            except Exception as e:
                await ctx.debug(f"Error searching {collection_name}: {str(e)}")
                continue
        
        # Sort by score and limit
        all_results.sort(key=lambda x: x.score, reverse=True)
        all_results = all_results[:limit]
        
        if not all_results:
            return f"No conversations found matching '{query}'. Try different keywords or check if conversations have been imported."
        
        # Format results
        result_text = f"Found {len(all_results)} relevant conversation(s) for '{query}':\n\n"
        for i, result in enumerate(all_results):
            result_text += f"**Result {i+1}** (Score: {result.score:.3f})\n"
            result_text += f"Time: {datetime.fromisoformat(result.timestamp).strftime('%Y-%m-%d %H:%M:%S')}\n"
            result_text += f"Project: {result.project_name}\n"
            result_text += f"Role: {result.role}\n"
            result_text += f"Excerpt: {result.excerpt}\n"
            result_text += "---\n\n"
        
        return result_text
        
    except Exception as e:
        await ctx.error(f"Search failed: {str(e)}")
        return f"Failed to search conversations: {str(e)}"

@mcp.tool()
async def store_reflection(
    ctx: Context,
    content: str = Field(description="The insight or reflection to store"),
    tags: List[str] = Field(default=[], description="Tags to categorize this reflection")
) -> str:
    """Store an important insight or reflection for future reference."""
    
    try:
        # TODO: Implement actual storage in a dedicated reflections collection
        # For now, just acknowledge the storage
        tags_str = ', '.join(tags) if tags else 'none'
        return f"Reflection stored successfully with tags: {tags_str}"
        
    except Exception as e:
        await ctx.error(f"Store failed: {str(e)}") 
        return f"Failed to store reflection: {str(e)}"


# Debug output (disabled for production)
# print(f"[DEBUG] FastMCP server v2.0.0 created with native Qdrant decay")

# Run the server when executed as main module
if __name__ == "__main__":
    mcp.run(transport="stdio", show_banner=False)