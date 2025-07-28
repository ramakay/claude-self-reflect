"""Claude Reflect MCP Server with Memory Decay."""

import os
import asyncio
from pathlib import Path
from typing import Any, Optional, List, Dict, Union
from datetime import datetime
import json
import numpy as np

from fastmcp import FastMCP, Context
from pydantic import BaseModel, Field
from qdrant_client import AsyncQdrantClient, models
from qdrant_client.models import (
    PointStruct, VectorParams, Distance
)

# Try to import newer Qdrant API for native decay
try:
    from qdrant_client.models import (
        Query, Formula, Expression, MultExpression,
        ExpDecayExpression, DecayParamsExpression,
        SearchRequest, NamedQuery
    )
    NATIVE_DECAY_AVAILABLE = True
except ImportError:
    # Fall back to older API
    from qdrant_client.models import (
        FormulaQuery, DecayParamsExpression, SumExpression,
        DatetimeExpression, DatetimeKeyExpression
    )
    NATIVE_DECAY_AVAILABLE = False
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
USE_NATIVE_DECAY = os.getenv('USE_NATIVE_DECAY', 'false').lower() == 'true'

# Embedding configuration
PREFER_LOCAL_EMBEDDINGS = os.getenv('PREFER_LOCAL_EMBEDDINGS', 'false').lower() == 'true'
EMBEDDING_MODEL = os.getenv('EMBEDDING_MODEL', 'sentence-transformers/all-MiniLM-L6-v2')

# Initialize Voyage AI client (only if not using local embeddings)
voyage_client = None
if not PREFER_LOCAL_EMBEDDINGS and VOYAGE_API_KEY:
    voyage_client = voyageai.Client(api_key=VOYAGE_API_KEY)

# Initialize local embedding model if needed
local_embedding_model = None
if PREFER_LOCAL_EMBEDDINGS or not VOYAGE_API_KEY:
    try:
        from fastembed import TextEmbedding
        local_embedding_model = TextEmbedding(model_name=EMBEDDING_MODEL)
        print(f"[DEBUG] Initialized local embedding model: {EMBEDDING_MODEL}")
    except ImportError:
        print("[ERROR] FastEmbed not available. Install with: pip install fastembed")
        raise

# Debug environment loading
print(f"[DEBUG] Environment variables loaded:")
print(f"[DEBUG] ENABLE_MEMORY_DECAY: {ENABLE_MEMORY_DECAY}")
print(f"[DEBUG] USE_NATIVE_DECAY: {USE_NATIVE_DECAY}")
print(f"[DEBUG] DECAY_WEIGHT: {DECAY_WEIGHT}")
print(f"[DEBUG] DECAY_SCALE_DAYS: {DECAY_SCALE_DAYS}")
print(f"[DEBUG] PREFER_LOCAL_EMBEDDINGS: {PREFER_LOCAL_EMBEDDINGS}")
print(f"[DEBUG] EMBEDDING_MODEL: {EMBEDDING_MODEL}")
print(f"[DEBUG] env_path: {env_path}")


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
    name="claude-self-reflect",
    instructions="Search past conversations and store reflections with time-based memory decay"
)

# Create Qdrant client
qdrant_client = AsyncQdrantClient(url=QDRANT_URL)
    
async def get_all_collections() -> List[str]:
    """Get all collections (both Voyage and local)."""
    collections = await qdrant_client.get_collections()
    # Support both _voyage and _local collections, plus reflections
    return [c.name for c in collections.collections 
            if c.name.endswith('_voyage') or c.name.endswith('_local') or c.name.startswith('reflections')]

async def generate_embedding(text: str) -> List[float]:
    """Generate embedding using configured provider."""
    if PREFER_LOCAL_EMBEDDINGS or not voyage_client:
        # Use local embeddings
        if not local_embedding_model:
            raise ValueError("Local embedding model not initialized")
        
        # Run in executor since fastembed is synchronous
        loop = asyncio.get_event_loop()
        embeddings = await loop.run_in_executor(
            None, lambda: list(local_embedding_model.embed([text]))
        )
        return embeddings[0].tolist()
    else:
        # Use Voyage AI
        result = voyage_client.embed(
            texts=[text],
            model="voyage-3-large",
            input_type="query"
        )
        return result.embeddings[0]

def get_embedding_dimension() -> int:
    """Get the dimension of embeddings based on the provider."""
    if PREFER_LOCAL_EMBEDDINGS or not voyage_client:
        # all-MiniLM-L6-v2 produces 384-dimensional embeddings
        return 384
    else:
        # voyage-3-large produces 1024-dimensional embeddings
        return 1024

def get_collection_suffix() -> str:
    """Get the collection suffix based on embedding provider."""
    if PREFER_LOCAL_EMBEDDINGS or not voyage_client:
        return "_local"
    else:
        return "_voyage"
    
# Register tools
@mcp.tool()
async def reflect_on_past(
    ctx: Context,
    query: str = Field(description="The search query to find semantically similar conversations"),
    limit: int = Field(default=5, description="Maximum number of results to return"),
    min_score: float = Field(default=0.7, description="Minimum similarity score (0-1)"),
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
    await ctx.debug(f"Native decay mode: {USE_NATIVE_DECAY}")
    await ctx.debug(f"ENABLE_MEMORY_DECAY env: {ENABLE_MEMORY_DECAY}")
    await ctx.debug(f"DECAY_WEIGHT: {DECAY_WEIGHT}, DECAY_SCALE_DAYS: {DECAY_SCALE_DAYS}")
    
    try:
        # Generate embedding
        query_embedding = await generate_embedding(query)
        
        # Get all collections
        all_collections = await get_all_collections()
        if not all_collections:
            return "No conversation collections found. Please import conversations first."
        
        await ctx.debug(f"Searching across {len(all_collections)} collections")
        await ctx.debug(f"Using {'local' if PREFER_LOCAL_EMBEDDINGS or not voyage_client else 'Voyage AI'} embeddings")
        
        all_results = []
        
        # Search each collection
        for collection_name in all_collections:
            try:
                if should_use_decay and USE_NATIVE_DECAY and NATIVE_DECAY_AVAILABLE:
                    # Use native Qdrant decay with newer API
                    await ctx.debug(f"Using NATIVE Qdrant decay (new API) for {collection_name}")
                    
                    # Build the query with native Qdrant decay formula using newer API
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
                    
                    # Execute query with native decay (new API)
                    results = await qdrant_client.query_points(
                        collection_name=collection_name,
                        query=query_obj,
                        limit=limit,
                        score_threshold=min_score,
                        with_payload=True
                    )
                elif should_use_decay and USE_NATIVE_DECAY and not NATIVE_DECAY_AVAILABLE:
                    # Use native Qdrant decay with older API
                    await ctx.debug(f"Using NATIVE Qdrant decay (legacy API) for {collection_name}")
                    
                    # Build the query with native Qdrant decay formula using older API
                    query_obj = FormulaQuery(
                        nearest=query_embedding,
                        formula=SumExpression(
                            sum=[
                                # Original similarity score
                                'score',  # Variable expression can be a string
                                # Decay boost term
                                {
                                    'mult': [
                                        # Decay weight (constant as float)
                                        DECAY_WEIGHT,
                                        # Exponential decay function
                                        {
                                            'exp_decay': DecayParamsExpression(
                                                # Use timestamp field for decay
                                                x=DatetimeKeyExpression(datetime_key='timestamp'),
                                                # Decay from current time (server-side)
                                                target=DatetimeExpression(datetime='now'),
                                                # Scale in milliseconds
                                                scale=DECAY_SCALE_DAYS * 24 * 60 * 60 * 1000,
                                                # Standard exponential decay midpoint
                                                midpoint=0.5
                                            )
                                        }
                                    ]
                                }
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
                    
                    # Process results from native decay search
                    for point in results.points:
                        # Clean timestamp for proper parsing
                        raw_timestamp = point.payload.get('timestamp', datetime.now().isoformat())
                        clean_timestamp = raw_timestamp.replace('Z', '+00:00') if raw_timestamp.endswith('Z') else raw_timestamp
                        
                        all_results.append(SearchResult(
                            id=str(point.id),
                            score=point.score,  # Score already includes decay
                            timestamp=clean_timestamp,
                            role=point.payload.get('start_role', point.payload.get('role', 'unknown')),
                            excerpt=(point.payload.get('text', '')[:500] + '...'),
                            project_name=point.payload.get('project', collection_name.replace('conv_', '').replace('_voyage', '').replace('_local', '')),
                            conversation_id=point.payload.get('conversation_id'),
                            collection_name=collection_name
                        ))
                    
                elif should_use_decay:
                    # Use client-side decay (existing implementation)
                    await ctx.debug(f"Using CLIENT-SIDE decay for {collection_name}")
                    
                    # Search without score threshold to get all candidates
                    results = await qdrant_client.search(
                        collection_name=collection_name,
                        query_vector=query_embedding,
                        limit=limit * 3,  # Get more candidates for decay filtering
                        with_payload=True
                    )
                    
                    # Apply decay scoring manually
                    now = datetime.now()
                    scale_ms = DECAY_SCALE_DAYS * 24 * 60 * 60 * 1000
                    
                    decay_results = []
                    for point in results:
                        try:
                            # Get timestamp from payload
                            timestamp_str = point.payload.get('timestamp')
                            if timestamp_str:
                                timestamp = datetime.fromisoformat(timestamp_str.replace('Z', '+00:00'))
                                age_ms = (now - timestamp).total_seconds() * 1000
                                
                                # Calculate decay factor
                                decay_factor = np.exp(-age_ms / scale_ms)
                                
                                # Apply decay formula
                                adjusted_score = point.score + (DECAY_WEIGHT * decay_factor)
                                
                                # Debug: show the calculation
                                age_days = age_ms / (24 * 60 * 60 * 1000)
                                await ctx.debug(f"Point: age={age_days:.1f} days, original_score={point.score:.3f}, decay_factor={decay_factor:.3f}, adjusted_score={adjusted_score:.3f}")
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
                        # Clean timestamp for proper parsing
                        raw_timestamp = point.payload.get('timestamp', datetime.now().isoformat())
                        clean_timestamp = raw_timestamp.replace('Z', '+00:00') if raw_timestamp.endswith('Z') else raw_timestamp
                        
                        all_results.append(SearchResult(
                            id=str(point.id),
                            score=adjusted_score,  # Use adjusted score
                            timestamp=clean_timestamp,
                            role=point.payload.get('start_role', point.payload.get('role', 'unknown')),
                            excerpt=(point.payload.get('text', '')[:500] + '...'),
                            project_name=point.payload.get('project', collection_name.replace('conv_', '').replace('_voyage', '').replace('_local', '')),
                            conversation_id=point.payload.get('conversation_id'),
                            collection_name=collection_name
                        ))
                else:
                    # Standard search without decay
                    results = await qdrant_client.search(
                        collection_name=collection_name,
                        query_vector=query_embedding,
                        limit=limit,
                        score_threshold=min_score,
                        with_payload=True
                    )
                    
                    for point in results:
                        # Clean timestamp for proper parsing
                        raw_timestamp = point.payload.get('timestamp', datetime.now().isoformat())
                        clean_timestamp = raw_timestamp.replace('Z', '+00:00') if raw_timestamp.endswith('Z') else raw_timestamp
                        
                        all_results.append(SearchResult(
                            id=str(point.id),
                            score=point.score,
                            timestamp=clean_timestamp,
                            role=point.payload.get('start_role', point.payload.get('role', 'unknown')),
                            excerpt=(point.payload.get('text', '')[:500] + '...'),
                            project_name=point.payload.get('project', collection_name.replace('conv_', '').replace('_voyage', '').replace('_local', '')),
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
            # Handle timezone suffix 'Z' properly
            timestamp_clean = result.timestamp.replace('Z', '+00:00') if result.timestamp.endswith('Z') else result.timestamp
            result_text += f"Time: {datetime.fromisoformat(timestamp_clean).strftime('%Y-%m-%d %H:%M:%S')}\n"
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
        # Create reflections collection name
        collection_name = f"reflections{get_collection_suffix()}"
        
        # Ensure collection exists
        try:
            collection_info = await qdrant_client.get_collection(collection_name)
        except:
            # Create collection if it doesn't exist
            await qdrant_client.create_collection(
                collection_name=collection_name,
                vectors_config=VectorParams(
                    size=get_embedding_dimension(),
                    distance=Distance.COSINE
                )
            )
            await ctx.debug(f"Created reflections collection: {collection_name}")
        
        # Generate embedding for the reflection
        embedding = await generate_embedding(content)
        
        # Create point with metadata
        point_id = datetime.now().timestamp()
        point = PointStruct(
            id=int(point_id),
            vector=embedding,
            payload={
                "text": content,
                "tags": tags,
                "timestamp": datetime.now().isoformat(),
                "type": "reflection",
                "role": "user_reflection"
            }
        )
        
        # Store in Qdrant
        await qdrant_client.upsert(
            collection_name=collection_name,
            points=[point]
        )
        
        tags_str = ', '.join(tags) if tags else 'none'
        return f"Reflection stored successfully with tags: {tags_str}"
        
    except Exception as e:
        await ctx.error(f"Store failed: {str(e)}")
        return f"Failed to store reflection: {str(e)}"


# Debug output
print(f"[DEBUG] FastMCP server created with name: {mcp.name}")
