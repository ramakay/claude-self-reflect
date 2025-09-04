"""Claude Reflect MCP Server with Memory Decay."""

import os
import asyncio
from pathlib import Path
from typing import Any, Optional, List, Dict, Union
from datetime import datetime, timezone
import json
import numpy as np
import hashlib
import time
import logging
from xml.sax.saxutils import escape

from fastmcp import FastMCP, Context
from .utils import normalize_project_name
from .project_resolver import ProjectResolver
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

# Load environment variables from .env file (fallback only)
env_path = Path(__file__).parent.parent.parent / '.env'
load_dotenv(env_path, override=False)  # Don't override process environment

# Configuration - prioritize process environment variables over .env file
QDRANT_URL = os.getenv('QDRANT_URL', 'http://localhost:6333')
VOYAGE_API_KEY = os.getenv('VOYAGE_KEY') or os.getenv('VOYAGE_KEY-2') or os.getenv('VOYAGE_KEY_2')
ENABLE_MEMORY_DECAY = os.getenv('ENABLE_MEMORY_DECAY', 'false').lower() == 'true'
DECAY_WEIGHT = float(os.getenv('DECAY_WEIGHT', '0.3'))

# Setup file logging
LOG_FILE = Path.home() / '.claude-self-reflect' / 'logs' / 'mcp-server.log'
LOG_FILE.parent.mkdir(parents=True, exist_ok=True)

# Configure logging to both file and console
logging.basicConfig(
    level=logging.DEBUG,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s',
    handlers=[
        logging.FileHandler(LOG_FILE, mode='a'),
        logging.StreamHandler()
    ]
)
DECAY_SCALE_DAYS = float(os.getenv('DECAY_SCALE_DAYS', '90'))
USE_NATIVE_DECAY = os.getenv('USE_NATIVE_DECAY', 'false').lower() == 'true'

# Embedding configuration - now using lazy initialization
# CRITICAL: Default changed to 'true' for local embeddings for privacy
PREFER_LOCAL_EMBEDDINGS = os.getenv('PREFER_LOCAL_EMBEDDINGS', 'true').lower() == 'true'
EMBEDDING_MODEL = os.getenv('EMBEDDING_MODEL', 'sentence-transformers/all-MiniLM-L6-v2')

# Import the robust embedding manager
from .embedding_manager import get_embedding_manager

# Lazy initialization - models will be loaded on first use
embedding_manager = None
voyage_client = None  # Keep for backward compatibility
local_embedding_model = None  # Keep for backward compatibility

def initialize_embeddings():
    """Initialize embedding models with robust fallback."""
    global embedding_manager, voyage_client, local_embedding_model
    try:
        embedding_manager = get_embedding_manager()
        print(f"[INFO] Embedding manager initialized: {embedding_manager.get_model_info()}")
        
        # Set backward compatibility references
        if embedding_manager.model_type == 'voyage':
            voyage_client = embedding_manager.voyage_client
        elif embedding_manager.model_type == 'local':
            local_embedding_model = embedding_manager.model
        
        return True
    except Exception as e:
        print(f"[ERROR] Failed to initialize embeddings: {e}")
        return False

# Debug environment loading and startup
import sys
import datetime as dt
startup_time = dt.datetime.now().isoformat()
print(f"[STARTUP] MCP Server starting at {startup_time}", file=sys.stderr)
print(f"[STARTUP] Python: {sys.version}", file=sys.stderr)
print(f"[STARTUP] Working directory: {os.getcwd()}", file=sys.stderr)
print(f"[STARTUP] Script location: {__file__}", file=sys.stderr)
print(f"[DEBUG] Environment variables loaded:", file=sys.stderr)
print(f"[DEBUG] QDRANT_URL: {QDRANT_URL}", file=sys.stderr)
print(f"[DEBUG] ENABLE_MEMORY_DECAY: {ENABLE_MEMORY_DECAY}", file=sys.stderr)
print(f"[DEBUG] USE_NATIVE_DECAY: {USE_NATIVE_DECAY}", file=sys.stderr)
print(f"[DEBUG] DECAY_WEIGHT: {DECAY_WEIGHT}", file=sys.stderr)
print(f"[DEBUG] DECAY_SCALE_DAYS: {DECAY_SCALE_DAYS}", file=sys.stderr)
print(f"[DEBUG] PREFER_LOCAL_EMBEDDINGS: {PREFER_LOCAL_EMBEDDINGS}", file=sys.stderr)
print(f"[DEBUG] EMBEDDING_MODEL: {EMBEDDING_MODEL}", file=sys.stderr)
print(f"[DEBUG] env_path: {env_path}", file=sys.stderr)


class SearchResult(BaseModel):
    """A single search result with pattern intelligence."""
    id: str
    score: float
    timestamp: str
    role: str
    excerpt: str
    project_name: str
    conversation_id: Optional[str] = None
    base_conversation_id: Optional[str] = None
    collection_name: str
    raw_payload: Optional[Dict[str, Any]] = None  # Full Qdrant payload when debug mode enabled
    # Pattern intelligence fields
    code_patterns: Optional[Dict[str, List[str]]] = None  # Extracted AST patterns
    files_analyzed: Optional[List[str]] = None  # Files referenced in conversation
    tools_used: Optional[List[str]] = None  # Tools/commands used
    concepts: Optional[List[str]] = None  # Domain concepts discussed


# Initialize FastMCP instance
mcp = FastMCP(
    name="claude-self-reflect",
    instructions="Search past conversations and store reflections with time-based memory decay"
)

# Create Qdrant client
qdrant_client = AsyncQdrantClient(url=QDRANT_URL)

# Track indexing status (updated periodically)
indexing_status = {
    "last_check": 0,
    "indexed_conversations": 0,
    "total_conversations": 0,
    "percentage": 100.0,
    "backlog_count": 0,
    "is_checking": False
}

# Cache for indexing status (5-second TTL)
_indexing_cache = {"result": None, "timestamp": 0}

# Setup logger
logger = logging.getLogger(__name__)
logger.info(f"MCP Server starting - Log file: {LOG_FILE}")
logger.info(f"Configuration: QDRANT_URL={QDRANT_URL}, DECAY={ENABLE_MEMORY_DECAY}, VOYAGE_API_STATUS={'Configured' if VOYAGE_API_KEY else 'Not Configured'}")

def normalize_path(path_str: str) -> str:
    """Normalize path for consistent comparison across platforms.
    
    Args:
        path_str: Path string to normalize
        
    Returns:
        Normalized path string with consistent separators
    """
    if not path_str:
        return path_str
    p = Path(path_str).expanduser().resolve()
    return str(p).replace('\\', '/')  # Consistent separators for all platforms

async def update_indexing_status(cache_ttl: int = 5):
    """Update indexing status by checking JSONL files vs Qdrant collections.
    This is a lightweight check that compares file counts, not full content.
    
    Args:
        cache_ttl: Cache time-to-live in seconds (default: 5)
    """
    global indexing_status, _indexing_cache
    
    # Check cache first (5-second TTL to prevent performance issues)
    current_time = time.time()
    if _indexing_cache["result"] and current_time - _indexing_cache["timestamp"] < cache_ttl:
        # Use cached result
        indexing_status = _indexing_cache["result"].copy()
        return
    
    # Don't run concurrent checks
    if indexing_status["is_checking"]:
        return
    
    # Check immediately on first call, then every 60 seconds to avoid overhead
    if indexing_status["last_check"] > 0 and current_time - indexing_status["last_check"] < 60:  # 1 minute
        return
    
    indexing_status["is_checking"] = True
    
    try:
        # Count total JSONL files
        projects_dir = Path.home() / ".claude" / "projects"
        total_files = 0
        indexed_files = 0
        
        if projects_dir.exists():
            # Get all JSONL files
            jsonl_files = list(projects_dir.glob("**/*.jsonl"))
            total_files = len(jsonl_files)
            
            # Check imported-files.json AND watcher state files to see what's been imported
            # The system uses multiple state files that need to be merged
            all_imported_files = set()  # Use set to avoid duplicates
            file_metadata = {}
            
            # 1. Check imported-files.json (batch importer)
            possible_paths = [
                Path.home() / ".claude-self-reflect" / "config" / "imported-files.json",
                Path(__file__).parent.parent.parent / "config" / "imported-files.json",
                Path("/config/imported-files.json")  # Docker path if running in container
            ]
            
            for path in possible_paths:
                if path.exists():
                    try:
                        with open(path, 'r') as f:
                            imported_data = json.load(f)
                            imported_files_dict = imported_data.get("imported_files", {})
                            file_metadata.update(imported_data.get("file_metadata", {}))
                            # Normalize paths before adding to set
                            normalized_files = {normalize_path(k) for k in imported_files_dict.keys()}
                            all_imported_files.update(normalized_files)
                    except (json.JSONDecodeError, IOError) as e:
                        logger.debug(f"Failed to read state file {path}: {e}")
                        pass  # Continue if file is corrupted
            
            # 2. Check csr-watcher.json (streaming watcher - local mode)
            watcher_paths = [
                Path.home() / ".claude-self-reflect" / "config" / "csr-watcher.json",
                Path("/config/csr-watcher.json")  # Docker path
            ]
            
            for path in watcher_paths:
                if path.exists():
                    try:
                        with open(path, 'r') as f:
                            watcher_data = json.load(f)
                            watcher_files = watcher_data.get("imported_files", {})
                            # Normalize paths before adding to set
                            normalized_files = {normalize_path(k) for k in watcher_files.keys()}
                            all_imported_files.update(normalized_files)
                            # Add to metadata with normalized paths
                            for file_path, info in watcher_files.items():
                                normalized = normalize_path(file_path)
                                if normalized not in file_metadata:
                                    file_metadata[normalized] = {
                                        "position": 1,
                                        "chunks": info.get("chunks", 0)
                                    }
                    except (json.JSONDecodeError, IOError) as e:
                        logger.debug(f"Failed to read watcher state file {path}: {e}")
                        pass  # Continue if file is corrupted
            
            # 3. Check csr-watcher-cloud.json (streaming watcher - cloud mode)
            cloud_watcher_path = Path.home() / ".claude-self-reflect" / "config" / "csr-watcher-cloud.json"
            if cloud_watcher_path.exists():
                try:
                    with open(cloud_watcher_path, 'r') as f:
                        cloud_data = json.load(f)
                        cloud_files = cloud_data.get("imported_files", {})
                        # Normalize paths before adding to set
                        normalized_files = {normalize_path(k) for k in cloud_files.keys()}
                        all_imported_files.update(normalized_files)
                        # Add to metadata with normalized paths
                        for file_path, info in cloud_files.items():
                            normalized = normalize_path(file_path)
                            if normalized not in file_metadata:
                                file_metadata[normalized] = {
                                    "position": 1,
                                    "chunks": info.get("chunks", 0)
                                }
                except (json.JSONDecodeError, IOError) as e:
                    logger.debug(f"Failed to read cloud watcher state file {cloud_watcher_path}: {e}")
                    pass  # Continue if file is corrupted
            
            # Convert set to list for compatibility
            imported_files_list = list(all_imported_files)
            
            # Count files that have been imported
            for file_path in jsonl_files:
                # Normalize the current file path for consistent comparison
                normalized_file = normalize_path(str(file_path))
                
                # Try multiple path formats to match Docker's state file
                file_str = str(file_path).replace(str(Path.home()), "/logs").replace("\\", "/")
                # Also try without .claude/projects prefix (Docker mounts directly)
                file_str_alt = file_str.replace("/.claude/projects", "")
                
                # Normalize alternative paths as well
                normalized_alt = normalize_path(file_str)
                normalized_alt2 = normalize_path(file_str_alt)
                
                # Check if file is in imported_files list (fully imported)
                if normalized_file in imported_files_list or normalized_alt in imported_files_list or normalized_alt2 in imported_files_list:
                    indexed_files += 1
                # Or if it has metadata with position > 0 (partially imported)
                elif normalized_file in file_metadata and file_metadata[normalized_file].get("position", 0) > 0:
                    indexed_files += 1
                elif normalized_alt in file_metadata and file_metadata[normalized_alt].get("position", 0) > 0:
                    indexed_files += 1
                elif normalized_alt2 in file_metadata and file_metadata[normalized_alt2].get("position", 0) > 0:
                    indexed_files += 1
        
        # Update status
        indexing_status["last_check"] = current_time
        indexing_status["total_conversations"] = total_files
        indexing_status["indexed_conversations"] = indexed_files
        indexing_status["backlog_count"] = total_files - indexed_files
        
        if total_files > 0:
            indexing_status["percentage"] = (indexed_files / total_files) * 100
        else:
            indexing_status["percentage"] = 100.0
        
        # Update cache
        _indexing_cache["result"] = indexing_status.copy()
        _indexing_cache["timestamp"] = current_time
            
    except Exception as e:
        print(f"[WARNING] Failed to update indexing status: {e}")
        logger.error(f"Failed to update indexing status: {e}", exc_info=True)
    finally:
        indexing_status["is_checking"] = False
    
async def get_all_collections() -> List[str]:
    """Get all collections (both Voyage and local)."""
    collections = await qdrant_client.get_collections()
    # Support both _voyage and _local collections, plus reflections
    return [c.name for c in collections.collections 
            if c.name.endswith('_voyage') or c.name.endswith('_local') or c.name.startswith('reflections')]

async def generate_embedding(text: str, force_type: Optional[str] = None) -> List[float]:
    """Generate embedding using configured provider or forced type.
    
    Args:
        text: Text to embed
        force_type: Force specific embedding type ('local' or 'voyage')
    """
    global embedding_manager, voyage_client, local_embedding_model
    
    # Initialize on first use
    if embedding_manager is None:
        if not initialize_embeddings():
            raise RuntimeError("Failed to initialize any embedding model. Check logs for details.")
    
    # Determine which type to use
    if force_type:
        use_local = force_type == 'local'
    else:
        use_local = embedding_manager.model_type == 'local'
    
    if use_local:
        # Use local embeddings
        if not local_embedding_model:
            raise ValueError("Local embedding model not available")
        
        # Run in executor since fastembed is synchronous
        loop = asyncio.get_event_loop()
        embeddings = await loop.run_in_executor(
            None, lambda: list(local_embedding_model.embed([text]))
        )
        return embeddings[0].tolist()
    else:
        # Use Voyage AI
        if not voyage_client:
            raise ValueError("Voyage client not available")
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

def aggregate_pattern_intelligence(results: List[SearchResult]) -> Dict[str, Any]:
    """Aggregate pattern intelligence across search results."""
    
    # Initialize counters
    all_patterns = {}
    all_files = set()
    all_tools = set()
    all_concepts = set()
    pattern_by_category = {}
    
    for result in results:
        # Aggregate code patterns
        if result.code_patterns:
            for category, patterns in result.code_patterns.items():
                if category not in pattern_by_category:
                    pattern_by_category[category] = {}
                for pattern in patterns:
                    if pattern not in pattern_by_category[category]:
                        pattern_by_category[category][pattern] = 0
                    pattern_by_category[category][pattern] += 1
                    
                    # Overall pattern count
                    if pattern not in all_patterns:
                        all_patterns[pattern] = 0
                    all_patterns[pattern] += 1
        
        # Aggregate files
        if result.files_analyzed:
            all_files.update(result.files_analyzed)
        
        # Aggregate tools
        if result.tools_used:
            all_tools.update(result.tools_used)
        
        # Aggregate concepts
        if result.concepts:
            all_concepts.update(result.concepts)
    
    # Find most common patterns
    sorted_patterns = sorted(all_patterns.items(), key=lambda x: x[1], reverse=True)
    most_common_patterns = sorted_patterns[:10] if sorted_patterns else []
    
    # Find pattern categories with most coverage
    category_coverage = {
        cat: sum(counts.values()) 
        for cat, counts in pattern_by_category.items()
    }
    
    # Build intelligence summary
    intelligence = {
        "total_unique_patterns": len(all_patterns),
        "most_common_patterns": most_common_patterns,
        "pattern_categories": list(pattern_by_category.keys()),
        "category_coverage": category_coverage,
        "files_referenced": list(all_files)[:20],  # Limit to top 20
        "tools_used": list(all_tools),
        "concepts_discussed": list(all_concepts)[:15],  # Limit to top 15
        "pattern_by_category": pattern_by_category,
        "pattern_diversity_score": len(all_patterns) / max(len(results), 1)  # Patterns per result
    }
    
    # Add cross-pattern insights
    if pattern_by_category:
        # Check for common pattern combinations
        async_error_combo = (
            'async_patterns' in pattern_by_category and 
            'error_handling' in pattern_by_category
        )
        react_state_combo = (
            'react_hooks' in pattern_by_category and
            any('useState' in p for p in pattern_by_category.get('react_hooks', {}).keys())
        )
        
        intelligence["pattern_combinations"] = {
            "async_with_error_handling": async_error_combo,
            "react_with_state": react_state_combo
        }
    
    return intelligence
    
# Register tools
@mcp.tool()
async def reflect_on_past(
    ctx: Context,
    query: str = Field(description="The search query to find semantically similar conversations"),
    limit: int = Field(default=5, description="Maximum number of results to return"),
    min_score: float = Field(default=0.7, description="Minimum similarity score (0-1)"),
    use_decay: Union[int, str] = Field(default=-1, description="Apply time-based decay: 1=enable, 0=disable, -1=use environment default (accepts int or str)"),
    project: Optional[str] = Field(default=None, description="Search specific project only. If not provided, searches current project based on working directory. Use 'all' to search across all projects."),
    include_raw: bool = Field(default=False, description="Include raw Qdrant payload data for debugging (increases response size)"),
    response_format: str = Field(default="xml", description="Response format: 'xml' or 'markdown'"),
    brief: bool = Field(default=False, description="Brief mode: returns minimal information for faster response")
) -> str:
    """Search for relevant past conversations using semantic search with optional time decay."""
    
    logger.info(f"=== SEARCH START === Query: '{query}', Project: '{project}', Limit: {limit}")
    
    # Start timing
    start_time = time.time()
    timing_info = {}
    
    # Normalize use_decay to integer
    timing_info['param_parsing_start'] = time.time()
    if isinstance(use_decay, str):
        try:
            use_decay = int(use_decay)
        except ValueError:
            raise ValueError("use_decay must be '1', '0', or '-1'")
    timing_info['param_parsing_end'] = time.time()
    
    # Parse decay parameter using integer approach
    should_use_decay = (
        True if use_decay == 1
        else False if use_decay == 0
        else ENABLE_MEMORY_DECAY  # -1 or any other value
    )
    
    # Determine project scope
    target_project = project
    
    # Always get the working directory for logging purposes
    cwd = os.environ.get('MCP_CLIENT_CWD', os.getcwd())
    
    if project is None:
        # Use MCP_CLIENT_CWD environment variable set by run-mcp.sh
        # This contains the actual working directory where Claude Code is running
        
        # Extract project name from path (e.g., /Users/.../projects/project-name)
        path_parts = Path(cwd).parts
        if 'projects' in path_parts:
            idx = path_parts.index('projects')
            if idx + 1 < len(path_parts):
                target_project = path_parts[idx + 1]
        elif '.claude' in path_parts:
            # If we're in a .claude directory, go up to find project
            for i, part in enumerate(path_parts):
                if part == '.claude' and i > 0:
                    target_project = path_parts[i - 1]
                    break
        
        # If still no project detected, use the last directory name
        if target_project is None:
            target_project = Path(cwd).name
    
    # For project matching, we need to handle the dash-encoded format
    # Convert folder name to the format used in stored data
    if target_project != 'all':
        # The stored format uses full path with dashes, so we need to construct it
        # For now, let's try to match based on the end of the project name
        pass  # We'll handle this differently in the filtering logic
    
    await ctx.debug(f"Searching for: {query}")
    await ctx.debug(f"Client working directory: {cwd}")
    await ctx.debug(f"Project scope: {target_project if target_project != 'all' else 'all projects'}")
    await ctx.debug(f"Decay enabled: {should_use_decay}")
    await ctx.debug(f"Native decay mode: {USE_NATIVE_DECAY}")
    await ctx.debug(f"ENABLE_MEMORY_DECAY env: {ENABLE_MEMORY_DECAY}")
    await ctx.debug(f"DECAY_WEIGHT: {DECAY_WEIGHT}, DECAY_SCALE_DAYS: {DECAY_SCALE_DAYS}")
    
    try:
        # We'll generate embeddings on-demand per collection type
        timing_info['embedding_prep_start'] = time.time()
        query_embeddings = {}  # Cache embeddings by type
        timing_info['embedding_prep_end'] = time.time()
        
        # Get all collections
        timing_info['get_collections_start'] = time.time()
        all_collections = await get_all_collections()
        timing_info['get_collections_end'] = time.time()
        
        if not all_collections:
            return "No conversation collections found. Please import conversations first."
        
        # Filter collections by project if not searching all
        project_collections = []  # Define at this scope for later use
        if target_project != 'all':
            # Use ProjectResolver to find collections for this project
            resolver = ProjectResolver(qdrant_client)
            project_collections = resolver.find_collections_for_project(target_project)
            
            if not project_collections:
                # Fall back to old method for backward compatibility
                normalized_name = normalize_project_name(target_project)
                project_hash = hashlib.md5(normalized_name.encode()).hexdigest()[:8]
                project_collections = [
                    c for c in all_collections 
                    if c.startswith(f"conv_{project_hash}_")
                ]
            
            # Always include reflections collections when searching a specific project
            reflections_collections = [c for c in all_collections if c.startswith('reflections')]
            
            if not project_collections:
                # Fall back to searching all collections but filtering by project metadata
                await ctx.debug(f"No collections found for project {target_project}, will filter by metadata")
                collections_to_search = all_collections
            else:
                await ctx.debug(f"Found {len(project_collections)} collections for project {target_project}")
                # Include both project collections and reflections
                collections_to_search = project_collections + reflections_collections
                # Remove duplicates
                collections_to_search = list(set(collections_to_search))
        else:
            collections_to_search = all_collections
        
        await ctx.debug(f"Searching across {len(collections_to_search)} collections")
        await ctx.debug(f"Using {'local' if PREFER_LOCAL_EMBEDDINGS or not voyage_client else 'Voyage AI'} embeddings")
        
        all_results = []
        
        # Search each collection
        timing_info['search_all_start'] = time.time()
        collection_timings = []
        
        # Report initial progress
        await ctx.report_progress(progress=0, total=len(collections_to_search))
        
        for idx, collection_name in enumerate(collections_to_search):
            collection_timing = {'name': collection_name, 'start': time.time()}
            
            # Report progress before searching each collection
            await ctx.report_progress(
                progress=idx, 
                total=len(collections_to_search),
                message=f"Searching {collection_name}"
            )
            
            try:
                # Determine embedding type for this collection
                embedding_type_for_collection = 'voyage' if collection_name.endswith('_voyage') else 'local'
                
                # Generate or retrieve cached embedding for this type
                if embedding_type_for_collection not in query_embeddings:
                    try:
                        query_embeddings[embedding_type_for_collection] = await generate_embedding(query, force_type=embedding_type_for_collection)
                    except Exception as e:
                        await ctx.debug(f"Failed to generate {embedding_type_for_collection} embedding: {e}")
                        continue
                
                query_embedding = query_embeddings[embedding_type_for_collection]
                
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
                        
                        # Check project filter if we're searching all collections but want specific project
                        point_project = point.payload.get('project', collection_name.replace('conv_', '').replace('_voyage', '').replace('_local', ''))
                        
                        # Special handling for reflections - they're global by default but can have project context
                        is_reflection_collection = collection_name.startswith('reflections')
                        
                        # Handle project matching - check if the target project name appears at the end of the stored project path
                        if target_project != 'all' and not project_collections and not is_reflection_collection:
                            # The stored project name is like "-Users-username-projects-ShopifyMCPMockShop"
                            # We want to match just "ShopifyMCPMockShop"
                            # Also handle underscore/dash variations (procsolve-website vs procsolve_website)
                            normalized_target = target_project.replace('-', '_')
                            normalized_stored = point_project.replace('-', '_')
                            if not (normalized_stored.endswith(f"_{normalized_target}") or 
                                    normalized_stored == normalized_target or
                                    point_project.endswith(f"-{target_project}") or 
                                    point_project == target_project):
                                continue  # Skip results from other projects
                        
                        # For reflections with project context, optionally filter by project
                        if is_reflection_collection and target_project != 'all' and 'project' in point.payload:
                            # Only filter if the reflection has project metadata
                            reflection_project = point.payload.get('project', '')
                            if reflection_project:
                                # Normalize both for comparison (handle underscore/dash variations)
                                normalized_target = target_project.replace('-', '_')
                                normalized_reflection = reflection_project.replace('-', '_')
                                if not (
                                    reflection_project == target_project or 
                                    normalized_reflection == normalized_target or
                                    reflection_project.endswith(f"/{target_project}") or
                                    reflection_project.endswith(f"-{target_project}") or
                                    normalized_reflection.endswith(f"_{normalized_target}") or
                                    normalized_reflection.endswith(f"/{normalized_target}")
                                ):
                                    continue  # Skip reflections from other projects
                            
                        # Log pattern data
                        patterns = point.payload.get('code_patterns')
                        logger.info(f"DEBUG: Creating SearchResult for point {point.id} from {collection_name}: has_patterns={bool(patterns)}, pattern_keys={list(patterns.keys()) if patterns else None}")
                        
                        all_results.append(SearchResult(
                            id=str(point.id),
                            score=point.score,  # Score already includes decay
                            timestamp=clean_timestamp,
                            role=point.payload.get('start_role', point.payload.get('role', 'unknown')),
                            excerpt=(point.payload.get('text', '')[:350] + '...' if len(point.payload.get('text', '')) > 350 else point.payload.get('text', '')),
                            project_name=point_project,
                            conversation_id=point.payload.get('conversation_id'),
                            base_conversation_id=point.payload.get('base_conversation_id'),
                            collection_name=collection_name,
                            raw_payload=point.payload,  # Always include payload for metadata extraction
                            # Pattern intelligence metadata
                            code_patterns=point.payload.get('code_patterns'),
                            files_analyzed=point.payload.get('files_analyzed'),
                            tools_used=list(point.payload.get('tools_used', [])) if isinstance(point.payload.get('tools_used'), set) else point.payload.get('tools_used'),
                            concepts=point.payload.get('concepts')
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
                    now = datetime.now(timezone.utc)
                    scale_ms = DECAY_SCALE_DAYS * 24 * 60 * 60 * 1000
                    
                    decay_results = []
                    for point in results:
                        try:
                            # Get timestamp from payload
                            timestamp_str = point.payload.get('timestamp')
                            if timestamp_str:
                                timestamp = datetime.fromisoformat(timestamp_str.replace('Z', '+00:00'))
                                # Ensure timestamp is timezone-aware
                                if timestamp.tzinfo is None:
                                    timestamp = timestamp.replace(tzinfo=timezone.utc)
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
                        
                        # Check project filter if we're searching all collections but want specific project
                        point_project = point.payload.get('project', collection_name.replace('conv_', '').replace('_voyage', '').replace('_local', ''))
                        
                        # Special handling for reflections - they're global by default but can have project context
                        is_reflection_collection = collection_name.startswith('reflections')
                        
                        # Handle project matching - check if the target project name appears at the end of the stored project path
                        if target_project != 'all' and not project_collections and not is_reflection_collection:
                            # The stored project name is like "-Users-username-projects-ShopifyMCPMockShop"
                            # We want to match just "ShopifyMCPMockShop"
                            # Also handle underscore/dash variations (procsolve-website vs procsolve_website)
                            normalized_target = target_project.replace('-', '_')
                            normalized_stored = point_project.replace('-', '_')
                            if not (normalized_stored.endswith(f"_{normalized_target}") or 
                                    normalized_stored == normalized_target or
                                    point_project.endswith(f"-{target_project}") or 
                                    point_project == target_project):
                                continue  # Skip results from other projects
                        
                        # For reflections with project context, optionally filter by project
                        if is_reflection_collection and target_project != 'all' and 'project' in point.payload:
                            # Only filter if the reflection has project metadata
                            reflection_project = point.payload.get('project', '')
                            if reflection_project:
                                # Normalize both for comparison (handle underscore/dash variations)
                                normalized_target = target_project.replace('-', '_')
                                normalized_reflection = reflection_project.replace('-', '_')
                                if not (
                                    reflection_project == target_project or 
                                    normalized_reflection == normalized_target or
                                    reflection_project.endswith(f"/{target_project}") or
                                    reflection_project.endswith(f"-{target_project}") or
                                    normalized_reflection.endswith(f"_{normalized_target}") or
                                    normalized_reflection.endswith(f"/{normalized_target}")
                                ):
                                    continue  # Skip reflections from other projects
                            
                        all_results.append(SearchResult(
                            id=str(point.id),
                            score=adjusted_score,  # Use adjusted score
                            timestamp=clean_timestamp,
                            role=point.payload.get('start_role', point.payload.get('role', 'unknown')),
                            excerpt=(point.payload.get('text', '')[:350] + '...' if len(point.payload.get('text', '')) > 350 else point.payload.get('text', '')),
                            project_name=point_project,
                            conversation_id=point.payload.get('conversation_id'),
                            base_conversation_id=point.payload.get('base_conversation_id'),
                            collection_name=collection_name,
                            raw_payload=point.payload,  # Always include payload for metadata extraction
                            # Pattern intelligence metadata
                            code_patterns=point.payload.get('code_patterns'),
                            files_analyzed=point.payload.get('files_analyzed'),
                            tools_used=list(point.payload.get('tools_used', [])) if isinstance(point.payload.get('tools_used'), set) else point.payload.get('tools_used'),
                            concepts=point.payload.get('concepts')
                        ))
                else:
                    # Standard search without decay
                    results = await qdrant_client.search(
                        collection_name=collection_name,
                        query_vector=query_embedding,
                        limit=limit * 2,  # Get more results to account for filtering
                        score_threshold=min_score * 0.9,  # Slightly lower threshold to catch v1 chunks
                        with_payload=True
                    )
                    
                    for point in results:
                        # Clean timestamp for proper parsing
                        raw_timestamp = point.payload.get('timestamp', datetime.now().isoformat())
                        clean_timestamp = raw_timestamp.replace('Z', '+00:00') if raw_timestamp.endswith('Z') else raw_timestamp
                        
                        # Check project filter if we're searching all collections but want specific project
                        point_project = point.payload.get('project', collection_name.replace('conv_', '').replace('_voyage', '').replace('_local', ''))
                        
                        # Special handling for reflections - they're global by default but can have project context
                        is_reflection_collection = collection_name.startswith('reflections')
                        
                        # Handle project matching - check if the target project name appears at the end of the stored project path
                        if target_project != 'all' and not project_collections and not is_reflection_collection:
                            # The stored project name is like "-Users-username-projects-ShopifyMCPMockShop"
                            # We want to match just "ShopifyMCPMockShop"
                            # Also handle underscore/dash variations (procsolve-website vs procsolve_website)
                            normalized_target = target_project.replace('-', '_')
                            normalized_stored = point_project.replace('-', '_')
                            if not (normalized_stored.endswith(f"_{normalized_target}") or 
                                    normalized_stored == normalized_target or
                                    point_project.endswith(f"-{target_project}") or 
                                    point_project == target_project):
                                continue  # Skip results from other projects
                        
                        # For reflections with project context, optionally filter by project
                        if is_reflection_collection and target_project != 'all' and 'project' in point.payload:
                            # Only filter if the reflection has project metadata
                            reflection_project = point.payload.get('project', '')
                            if reflection_project:
                                # Normalize both for comparison (handle underscore/dash variations)
                                normalized_target = target_project.replace('-', '_')
                                normalized_reflection = reflection_project.replace('-', '_')
                                if not (
                                    reflection_project == target_project or 
                                    normalized_reflection == normalized_target or
                                    reflection_project.endswith(f"/{target_project}") or
                                    reflection_project.endswith(f"-{target_project}") or
                                    normalized_reflection.endswith(f"_{normalized_target}") or
                                    normalized_reflection.endswith(f"/{normalized_target}")
                                ):
                                    continue  # Skip reflections from other projects
                        
                        # BOOST V2 CHUNKS: Apply score boost for v2 chunks (better quality)
                        original_score = point.score
                        final_score = original_score
                        chunking_version = point.payload.get('chunking_version', 'v1')
                        
                        if chunking_version == 'v2':
                            # Boost v2 chunks by 20% (configurable)
                            boost_factor = 1.2  # From migration config
                            final_score = min(1.0, original_score * boost_factor)
                            await ctx.debug(f"Boosted v2 chunk: {original_score:.3f} -> {final_score:.3f}")
                        
                        # Apply minimum score threshold after boosting
                        if final_score < min_score:
                            continue
                            
                        search_result = SearchResult(
                            id=str(point.id),
                            score=final_score,
                            timestamp=clean_timestamp,
                            role=point.payload.get('start_role', point.payload.get('role', 'unknown')),
                            excerpt=(point.payload.get('text', '')[:350] + '...' if len(point.payload.get('text', '')) > 350 else point.payload.get('text', '')),
                            project_name=point_project,
                            conversation_id=point.payload.get('conversation_id'),
                            base_conversation_id=point.payload.get('base_conversation_id'),
                            collection_name=collection_name,
                            raw_payload=point.payload,  # Always include payload for metadata extraction
                            # Pattern intelligence metadata
                            code_patterns=point.payload.get('code_patterns'),
                            files_analyzed=point.payload.get('files_analyzed'),
                            tools_used=list(point.payload.get('tools_used', [])) if isinstance(point.payload.get('tools_used'), set) else point.payload.get('tools_used'),
                            concepts=point.payload.get('concepts')
                        )
                        
                        all_results.append(search_result)
            
            except Exception as e:
                await ctx.debug(f"Error searching {collection_name}: {str(e)}")
                collection_timing['error'] = str(e)
            
            collection_timing['end'] = time.time()
            collection_timings.append(collection_timing)
        
        timing_info['search_all_end'] = time.time()
        
        # Report completion of search phase
        await ctx.report_progress(
            progress=len(collections_to_search), 
            total=len(collections_to_search),
            message="Search complete, processing results"
        )
        
        # Apply base_conversation_id boosting before sorting
        timing_info['boost_start'] = time.time()
        
        # Group results by base_conversation_id to identify related chunks
        base_conversation_groups = {}
        for result in all_results:
            base_id = result.base_conversation_id
            if base_id:
                if base_id not in base_conversation_groups:
                    base_conversation_groups[base_id] = []
                base_conversation_groups[base_id].append(result)
        
        # Apply boost to results from base conversations with multiple high-scoring chunks
        base_conversation_boost = 0.1  # Boost factor for base conversation matching
        for base_id, group_results in base_conversation_groups.items():
            if len(group_results) > 1:  # Multiple chunks from same base conversation
                avg_score = sum(r.score for r in group_results) / len(group_results)
                if avg_score > 0.8:  # Only boost high-quality base conversations
                    for result in group_results:
                        result.score += base_conversation_boost
                        await ctx.debug(f"Boosted result from base_conversation_id {base_id}: {result.score:.3f}")
        
        timing_info['boost_end'] = time.time()
        
        # Sort by score and limit
        timing_info['sort_start'] = time.time()
        all_results.sort(key=lambda x: x.score, reverse=True)
        all_results = all_results[:limit]
        timing_info['sort_end'] = time.time()
        
        logger.info(f"Total results: {len(all_results)}, Returning: {len(all_results[:limit])}")
        for r in all_results[:3]:  # Log first 3
            logger.debug(f"Result: id={r.id}, has_patterns={bool(r.code_patterns)}, pattern_keys={list(r.code_patterns.keys()) if r.code_patterns else None}")
        
        if not all_results:
            return f"No conversations found matching '{query}'. Try different keywords or check if conversations have been imported."
        
        # Aggregate pattern intelligence across results
        pattern_intelligence = aggregate_pattern_intelligence(all_results)
        
        # Update indexing status before returning results
        await update_indexing_status()
        
        # Format results based on response_format
        timing_info['format_start'] = time.time()
        
        if response_format == "xml":
            # Add upfront summary for immediate visibility (before collapsible XML)
            upfront_summary = ""
            
            # Show result summary
            if all_results:
                score_info = "high" if all_results[0].score >= 0.85 else "good" if all_results[0].score >= 0.75 else "partial"
                upfront_summary += f"🎯 RESULTS: {len(all_results)} matches ({score_info} relevance, top score: {all_results[0].score:.3f})\n"
                
                # Show performance with indexing status inline
                total_time = time.time() - start_time
                indexing_info = f" | 📊 {indexing_status['indexed_conversations']}/{indexing_status['total_conversations']} indexed" if indexing_status["percentage"] < 100.0 else ""
                upfront_summary += f"⚡ PERFORMANCE: {int(total_time * 1000)}ms ({len(collections_to_search)} collections searched{indexing_info})\n"
            else:
                upfront_summary += f"❌ NO RESULTS: No conversations found matching '{query}'\n"
            
            # XML format (compact tags for performance)
            result_text = upfront_summary + "\n<search>\n"
            
            # Add indexing status if not fully baselined - put key stats in opening tag for immediate visibility
            if indexing_status["percentage"] < 95.0:
                result_text += f'  <info status="indexing" progress="{indexing_status["percentage"]:.1f}%" backlog="{indexing_status["backlog_count"]}">\n'
                result_text += f'    <message>📊 Indexing: {indexing_status["indexed_conversations"]}/{indexing_status["total_conversations"]} conversations ({indexing_status["percentage"]:.1f}% complete, {indexing_status["backlog_count"]} pending)</message>\n'
                result_text += f"  </info>\n"
            
            # Add high-level result summary
            if all_results:
                # Count today's results
                now = datetime.now(timezone.utc)
                today_count = 0
                yesterday_count = 0
                week_count = 0
                
                for result in all_results:
                    timestamp_clean = result.timestamp.replace('Z', '+00:00') if result.timestamp.endswith('Z') else result.timestamp
                    timestamp_dt = datetime.fromisoformat(timestamp_clean)
                    if timestamp_dt.tzinfo is None:
                        timestamp_dt = timestamp_dt.replace(tzinfo=timezone.utc)
                    
                    days_ago = (now - timestamp_dt).days
                    if days_ago == 0:
                        today_count += 1
                    elif days_ago == 1:
                        yesterday_count += 1
                    if days_ago <= 7:
                        week_count += 1
                
                # Compact summary with key info in opening tag
                time_info = ""
                if today_count > 0:
                    time_info = f"{today_count} today"
                elif yesterday_count > 0:
                    time_info = f"{yesterday_count} yesterday"
                elif week_count > 0:
                    time_info = f"{week_count} this week"
                else:
                    time_info = "older results"
                
                score_info = "high" if all_results[0].score >= 0.85 else "good" if all_results[0].score >= 0.75 else "partial"
                
                result_text += f'  <summary count="{len(all_results)}" relevance="{score_info}" recency="{time_info}" top-score="{all_results[0].score:.3f}">\n'
                
                # Short preview of top result
                top_excerpt = all_results[0].excerpt[:100].strip()
                if '...' not in top_excerpt:
                    top_excerpt += "..."
                result_text += f'    <preview>{top_excerpt}</preview>\n'
                result_text += f"  </summary>\n"
            else:
                result_text += f"  <result-summary>\n"
                result_text += f"    <headline>No matches found</headline>\n"
                result_text += f"    <relevance>No conversations matched your query</relevance>\n"
                result_text += f"  </result-summary>\n"
            
            result_text += f"  <meta>\n"
            result_text += f"    <q>{query}</q>\n"
            result_text += f"    <scope>{target_project if target_project != 'all' else 'all'}</scope>\n"
            result_text += f"    <count>{len(all_results)}</count>\n"
            if all_results:
                result_text += f"    <range>{all_results[-1].score:.3f}-{all_results[0].score:.3f}</range>\n"
            result_text += f"    <embed>{'local' if PREFER_LOCAL_EMBEDDINGS or not voyage_client else 'voyage'}</embed>\n"
            
            # Add timing metadata
            total_time = time.time() - start_time
            result_text += f"    <perf>\n"
            result_text += f"      <ttl>{int(total_time * 1000)}</ttl>\n"
            result_text += f"      <emb>{int((timing_info.get('embedding_end', 0) - timing_info.get('embedding_start', 0)) * 1000)}</emb>\n"
            result_text += f"      <srch>{int((timing_info.get('search_all_end', 0) - timing_info.get('search_all_start', 0)) * 1000)}</srch>\n"
            result_text += f"      <cols>{len(collections_to_search)}</cols>\n"
            result_text += f"    </perf>\n"
            result_text += f"  </meta>\n"
            
            result_text += "  <results>\n"
            for i, result in enumerate(all_results):
                result_text += f'    <r rank="{i+1}">\n'
                result_text += f"      <s>{result.score:.3f}</s>\n"
                result_text += f"      <p>{result.project_name}</p>\n"
                
                # Calculate relative time
                timestamp_clean = result.timestamp.replace('Z', '+00:00') if result.timestamp.endswith('Z') else result.timestamp
                timestamp_dt = datetime.fromisoformat(timestamp_clean)
                # Ensure both datetimes are timezone-aware
                if timestamp_dt.tzinfo is None:
                    timestamp_dt = timestamp_dt.replace(tzinfo=timezone.utc)
                now = datetime.now(timezone.utc)
                days_ago = (now - timestamp_dt).days
                if days_ago == 0:
                    time_str = "today"
                elif days_ago == 1:
                    time_str = "yesterday"
                else:
                    time_str = f"{days_ago}d"
                result_text += f"      <t>{time_str}</t>\n"
                
                if not brief:
                    # Extract title from first line of excerpt
                    excerpt_lines = result.excerpt.split('\n')
                    title = excerpt_lines[0][:80] + "..." if len(excerpt_lines[0]) > 80 else excerpt_lines[0]
                    result_text += f"      <title>{title}</title>\n"
                    
                    # Key finding - summarize the main point
                    key_finding = result.excerpt[:100] + "..." if len(result.excerpt) > 100 else result.excerpt
                    result_text += f"      <key-finding>{key_finding.strip()}</key-finding>\n"
                
                # Always include excerpt, but shorter in brief mode
                if brief:
                    brief_excerpt = result.excerpt[:100] + "..." if len(result.excerpt) > 100 else result.excerpt
                    result_text += f"      <excerpt>{brief_excerpt.strip()}</excerpt>\n"
                else:
                    result_text += f"      <excerpt><![CDATA[{result.excerpt}]]></excerpt>\n"
                
                if result.conversation_id:
                    result_text += f"      <cid>{result.conversation_id}</cid>\n"
                
                # Include raw data if requested
                if include_raw and result.raw_payload:
                    result_text += "      <raw>\n"
                    result_text += f"        <txt><![CDATA[{result.raw_payload.get('text', '')}]]></txt>\n"
                    result_text += f"        <id>{result.id}</id>\n"
                    result_text += f"        <dist>{1 - result.score:.3f}</dist>\n"
                    result_text += "        <meta>\n"
                    for key, value in result.raw_payload.items():
                        if key != 'text':
                            result_text += f"          <{key}>{value}</{key}>\n"
                    result_text += "        </meta>\n"
                    result_text += "      </raw>\n"
                
                # Add patterns if they exist - with detailed logging
                if result.code_patterns and isinstance(result.code_patterns, dict):
                    logger.info(f"DEBUG: Point {result.id} has code_patterns dict with keys: {list(result.code_patterns.keys())}")
                    patterns_to_show = []
                    for category, patterns in result.code_patterns.items():
                        if patterns and isinstance(patterns, list) and len(patterns) > 0:
                            # Take up to 5 patterns from each category
                            patterns_to_show.append((category, patterns[:5]))
                            logger.info(f"DEBUG: Added category '{category}' with {len(patterns)} patterns")
                    
                    if patterns_to_show:
                        logger.info(f"DEBUG: Adding patterns XML for point {result.id}")
                        result_text += "      <patterns>\n"
                        for category, patterns in patterns_to_show:
                            # Escape both category name and pattern content for XML safety
                            safe_patterns = ', '.join(escape(str(p)) for p in patterns)
                            result_text += f"        <cat name=\"{escape(category)}\">{safe_patterns}</cat>\n"
                        result_text += "      </patterns>\n"
                    else:
                        logger.info(f"DEBUG: Point {result.id} has code_patterns but no valid patterns to show")
                else:
                    logger.info(f"DEBUG: Point {result.id} has no patterns. code_patterns={result.code_patterns}, type={type(result.code_patterns)}")
                
                if result.files_analyzed and len(result.files_analyzed) > 0:
                    result_text += f"      <files>{', '.join(result.files_analyzed[:5])}</files>\n"
                if result.concepts and len(result.concepts) > 0:
                    result_text += f"      <concepts>{', '.join(result.concepts[:5])}</concepts>\n"
                
                # Include structured metadata for agent consumption
                # This provides clean, parsed fields that agents can easily use
                if hasattr(result, 'raw_payload') and result.raw_payload:
                    import json
                    payload = result.raw_payload
                    
                    # Files section - structured for easy agent parsing
                    files_analyzed = payload.get('files_analyzed', [])
                    files_edited = payload.get('files_edited', [])
                    if files_analyzed or files_edited:
                        result_text += "      <files>\n"
                        if files_analyzed:
                            result_text += f"        <analyzed count=\"{len(files_analyzed)}\">"
                            result_text += ", ".join(files_analyzed[:5])  # First 5 files
                            if len(files_analyzed) > 5:
                                result_text += f" ... and {len(files_analyzed)-5} more"
                            result_text += "</analyzed>\n"
                        if files_edited:
                            result_text += f"        <edited count=\"{len(files_edited)}\">"
                            result_text += ", ".join(files_edited[:5])  # First 5 files
                            if len(files_edited) > 5:
                                result_text += f" ... and {len(files_edited)-5} more"
                            result_text += "</edited>\n"
                        result_text += "      </files>\n"
                    
                    # Concepts section - clean list for agents
                    concepts = payload.get('concepts', [])
                    if concepts:
                        result_text += f"      <concepts>{', '.join(concepts)}</concepts>\n"
                    
                    # Tools section - summarized with counts
                    tools_used = payload.get('tools_used', [])
                    if tools_used:
                        # Count tool usage
                        tool_counts = {}
                        for tool in tools_used:
                            tool_counts[tool] = tool_counts.get(tool, 0) + 1
                        # Sort by frequency
                        sorted_tools = sorted(tool_counts.items(), key=lambda x: x[1], reverse=True)
                        tool_summary = ", ".join(f"{tool}({count})" for tool, count in sorted_tools[:5])
                        if len(sorted_tools) > 5:
                            tool_summary += f" ... and {len(sorted_tools)-5} more"
                        result_text += f"      <tools>{tool_summary}</tools>\n"
                    
                    # Code patterns section - structured by category
                    code_patterns = payload.get('code_patterns', {})
                    if code_patterns:
                        result_text += "      <code_patterns>\n"
                        for category, patterns in code_patterns.items():
                            if patterns:
                                pattern_list = patterns if isinstance(patterns, list) else [patterns]
                                # Clean up pattern names
                                clean_patterns = []
                                for p in pattern_list[:5]:
                                    # Remove common prefixes like $FUNC, $VAR
                                    clean_p = str(p).replace('$FUNC', '').replace('$VAR', '').strip()
                                    if clean_p:
                                        clean_patterns.append(clean_p)
                                if clean_patterns:
                                    result_text += f"        <{category}>{', '.join(clean_patterns)}</{category}>\n"
                        result_text += "      </code_patterns>\n"
                    
                    # Pattern inheritance info - shows propagation details
                    pattern_inheritance = payload.get('pattern_inheritance', {})
                    if pattern_inheritance:
                        source_chunk = pattern_inheritance.get('source_chunk', '')
                        confidence = pattern_inheritance.get('confidence', 0)
                        distance = pattern_inheritance.get('distance', 0)
                        if source_chunk:
                            result_text += f"      <pattern_source chunk=\"{source_chunk}\" confidence=\"{confidence:.2f}\" distance=\"{distance}\"/>\n"
                    
                    # Message stats for context
                    msg_count = payload.get('message_count')
                    total_length = payload.get('total_length')
                    if msg_count or total_length:
                        stats_attrs = []
                        if msg_count:
                            stats_attrs.append(f'messages="{msg_count}"')
                        if total_length:
                            stats_attrs.append(f'length="{total_length}"')
                        result_text += f"      <stats {' '.join(stats_attrs)}/>\n"
                    
                    # Raw metadata dump for backwards compatibility
                    # Kept minimal - only truly unique fields
                    remaining_metadata = {}
                    excluded_keys = {'text', 'conversation_id', 'timestamp', 'role', 'project', 'chunk_index',
                                   'files_analyzed', 'files_edited', 'concepts', 'tools_used', 
                                   'code_patterns', 'pattern_inheritance', 'message_count', 'total_length',
                                   'chunking_version', 'chunk_method', 'chunk_overlap', 'migration_type'}
                    for key, value in payload.items():
                        if key not in excluded_keys and value is not None:
                            if isinstance(value, set):
                                value = list(value)
                            remaining_metadata[key] = value
                    
                    if remaining_metadata:
                        try:
                            # Only include if there's actually extra data
                            result_text += f"      <metadata_extra><![CDATA[{json.dumps(remaining_metadata, default=str)}]]></metadata_extra>\n"
                        except:
                            pass
                
                result_text += "    </r>\n"
            result_text += "  </results>\n"
            
            # Add aggregated pattern intelligence section
            if pattern_intelligence and pattern_intelligence.get('total_unique_patterns', 0) > 0:
                result_text += "  <pattern_intelligence>\n"
                
                # Summary statistics
                result_text += f"    <summary>\n"
                result_text += f"      <unique_patterns>{pattern_intelligence['total_unique_patterns']}</unique_patterns>\n"
                result_text += f"      <pattern_diversity>{pattern_intelligence['pattern_diversity_score']:.2f}</pattern_diversity>\n"
                result_text += f"    </summary>\n"
                
                # Most common patterns
                if pattern_intelligence.get('most_common_patterns'):
                    result_text += "    <common_patterns>\n"
                    for pattern, count in pattern_intelligence['most_common_patterns'][:5]:
                        result_text += f"      <pattern count=\"{count}\">{pattern}</pattern>\n"
                    result_text += "    </common_patterns>\n"
                
                # Pattern categories
                if pattern_intelligence.get('category_coverage'):
                    result_text += "    <categories>\n"
                    for category, count in pattern_intelligence['category_coverage'].items():
                        result_text += f"      <cat name=\"{category}\" count=\"{count}\"/>\n"
                    result_text += "    </categories>\n"
                
                # Pattern combinations insight
                if pattern_intelligence.get('pattern_combinations'):
                    combos = pattern_intelligence['pattern_combinations']
                    if combos.get('async_with_error_handling'):
                        result_text += "    <insight>Async patterns combined with error handling detected</insight>\n"
                    if combos.get('react_with_state'):
                        result_text += "    <insight>React hooks with state management patterns detected</insight>\n"
                
                # Files referenced across results
                if pattern_intelligence.get('files_referenced') and len(pattern_intelligence['files_referenced']) > 0:
                    result_text += f"    <files_across_results>{', '.join(pattern_intelligence['files_referenced'][:10])}</files_across_results>\n"
                
                # Concepts discussed
                if pattern_intelligence.get('concepts_discussed') and len(pattern_intelligence['concepts_discussed']) > 0:
                    result_text += f"    <concepts_discussed>{', '.join(pattern_intelligence['concepts_discussed'][:10])}</concepts_discussed>\n"
                
                result_text += "  </pattern_intelligence>\n"
            
            result_text += "</search>"
            
        else:
            # Markdown format (original)
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
        
        timing_info['format_end'] = time.time()
        
        # Log detailed timing breakdown
        await ctx.debug(f"\n=== TIMING BREAKDOWN ===")
        await ctx.debug(f"Total time: {(time.time() - start_time) * 1000:.1f}ms")
        await ctx.debug(f"Embedding generation: {(timing_info.get('embedding_end', 0) - timing_info.get('embedding_start', 0)) * 1000:.1f}ms")
        await ctx.debug(f"Get collections: {(timing_info.get('get_collections_end', 0) - timing_info.get('get_collections_start', 0)) * 1000:.1f}ms")
        await ctx.debug(f"Search all collections: {(timing_info.get('search_all_end', 0) - timing_info.get('search_all_start', 0)) * 1000:.1f}ms")
        await ctx.debug(f"Sorting results: {(timing_info.get('sort_end', 0) - timing_info.get('sort_start', 0)) * 1000:.1f}ms")
        await ctx.debug(f"Formatting output: {(timing_info.get('format_end', 0) - timing_info.get('format_start', 0)) * 1000:.1f}ms")
        
        # Log per-collection timings
        await ctx.debug(f"\n=== PER-COLLECTION TIMINGS ===")
        for ct in collection_timings:
            duration = (ct.get('end', 0) - ct.get('start', 0)) * 1000
            status = "ERROR" if 'error' in ct else "OK"
            await ctx.debug(f"{ct['name']}: {duration:.1f}ms ({status})")
        
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
        
        # Get current project context
        cwd = os.environ.get('MCP_CLIENT_CWD', os.getcwd())
        project_path = Path(cwd)
        
        # Extract project name from path
        project_name = None
        path_parts = project_path.parts
        if 'projects' in path_parts:
            idx = path_parts.index('projects')
            if idx + 1 < len(path_parts):
                # Get all parts after 'projects' to form the project name
                # This handles cases like projects/Connectiva-App/connectiva-ai
                project_parts = path_parts[idx + 1:]
                project_name = '/'.join(project_parts)
        
        # If no project detected, use the last directory name
        if not project_name:
            project_name = project_path.name
        
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
        
        # Create point with metadata including project context
        point_id = datetime.now().timestamp()
        point = PointStruct(
            id=int(point_id),
            vector=embedding,
            payload={
                "text": content,
                "tags": tags,
                "timestamp": datetime.now().isoformat(),
                "type": "reflection",
                "role": "user_reflection",
                "project": project_name,  # Add project context
                "project_path": str(project_path)  # Add full path for reference
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


@mcp.tool()
async def quick_search(
    ctx: Context,
    query: str = Field(description="The search query to find semantically similar conversations"),
    min_score: float = Field(default=0.7, description="Minimum similarity score (0-1)"),
    project: Optional[str] = Field(default=None, description="Search specific project only. If not provided, searches current project based on working directory. Use 'all' to search across all projects.")
) -> str:
    """Quick search that returns only the count and top result for fast overview."""
    # MCP architectural limitation: MCP tools cannot call other MCP tools
    return """<error>
MCP Architectural Limitation: This tool cannot directly call other MCP tools.

To perform a quick search, please:
1. Call reflect_on_past directly with limit=1 and brief=True
2. Or use the reflection-specialist agent for quick searches

This limitation exists because MCP tools can only be orchestrated by the client (Claude), 
not by other tools within the MCP server.
</error>"""


@mcp.tool()
async def search_summary(
    ctx: Context,
    query: str = Field(description="The search query to find semantically similar conversations"),
    project: Optional[str] = Field(default=None, description="Search specific project only. If not provided, searches current project based on working directory. Use 'all' to search across all projects.")
) -> str:
    """Get aggregated insights from search results without individual result details."""
    # MCP architectural limitation: MCP tools cannot call other MCP tools
    # This is a fundamental constraint of the MCP protocol
    return """<error>
MCP Architectural Limitation: This tool cannot directly call other MCP tools.

To get a search summary, please use the reflection-specialist agent instead:
1. Call the reflection-specialist agent
2. Ask it to provide a summary of search results for your query

Alternative: Call reflect_on_past directly and analyze the results yourself.

This limitation exists because MCP tools can only be orchestrated by the client (Claude), 
not by other tools within the MCP server.
</error>"""


@mcp.tool()
async def get_more_results(
    ctx: Context,
    query: str = Field(description="The original search query"),
    offset: int = Field(default=3, description="Number of results to skip (for pagination)"),
    limit: int = Field(default=3, description="Number of additional results to return"),
    min_score: float = Field(default=0.7, description="Minimum similarity score (0-1)"),
    project: Optional[str] = Field(default=None, description="Search specific project only")
) -> str:
    """Get additional search results after an initial search (pagination support)."""
    # MCP architectural limitation: MCP tools cannot call other MCP tools
    return """<error>
MCP Architectural Limitation: This tool cannot directly call other MCP tools.

To get more search results, please:
1. Call reflect_on_past directly with a higher limit parameter
2. Or use the reflection-specialist agent to handle pagination

This limitation exists because MCP tools can only be orchestrated by the client (Claude), 
not by other tools within the MCP server.
</error>"""


@mcp.tool()
async def search_by_file(
    ctx: Context,
    file_path: str = Field(description="The file path to search for in conversations"),
    limit: int = Field(default=10, description="Maximum number of results to return"),
    project: Optional[str] = Field(default=None, description="Search specific project only. Use 'all' to search across all projects.")
) -> str:
    """Search for conversations that analyzed a specific file."""
    global qdrant_client
    
    # Normalize file path
    normalized_path = file_path.replace("\\", "/").replace("/Users/", "~/")
    
    # Determine which collections to search
    # If no project specified, search all collections
    collections = await get_all_collections() if not project else []
    
    if project and project != 'all':
        # Filter collections for specific project
        project_hash = hashlib.md5(project.encode()).hexdigest()[:8]
        collection_prefix = f"conv_{project_hash}_"
        collections = [c for c in await get_all_collections() if c.startswith(collection_prefix)]
    elif project == 'all':
        collections = await get_all_collections()
    
    if not collections:
        return "<search_by_file>\n<error>No collections found to search</error>\n</search_by_file>"
    
    # Prepare results
    all_results = []
    
    for collection_name in collections:
        try:
            # Use scroll to get all points and filter manually
            # Qdrant's array filtering can be tricky, so we'll filter in code
            scroll_result = await qdrant_client.scroll(
                collection_name=collection_name,
                limit=1000,  # Get a batch
                with_payload=True
            )
            
            # Filter results that contain the file
            for point in scroll_result[0]:
                payload = point.payload
                files_analyzed = payload.get('files_analyzed', [])
                files_edited = payload.get('files_edited', [])
                
                # Check for exact match or if any file ends with the normalized path
                file_match = False
                for file in files_analyzed + files_edited:
                    if file == normalized_path or file.endswith('/' + normalized_path) or file.endswith('\\' + normalized_path):
                        file_match = True
                        break
                
                if file_match:
                    all_results.append({
                        'score': 1.0,  # File match is always 1.0
                        'payload': payload,
                        'collection': collection_name
                    })
                
        except Exception as e:
            continue
    
    # Sort by timestamp (newest first)
    all_results.sort(key=lambda x: x['payload'].get('timestamp', ''), reverse=True)
    
    # Format results
    if not all_results:
        return f"""<search_by_file>
<query>{file_path}</query>
<normalized_path>{normalized_path}</normalized_path>
<message>No conversations found that analyzed this file</message>
</search_by_file>"""
    
    results_text = []
    for i, result in enumerate(all_results[:limit]):
        payload = result['payload']
        timestamp = payload.get('timestamp', 'Unknown')
        conversation_id = payload.get('conversation_id', 'Unknown')
        project = payload.get('project', 'Unknown')
        text_preview = payload.get('text', '')[:200] + '...' if len(payload.get('text', '')) > 200 else payload.get('text', '')
        
        # Check if file was edited or just read
        action = "edited" if normalized_path in payload.get('files_edited', []) else "analyzed"
        
        # Get related tools used
        tool_summary = payload.get('tool_summary', {})
        tools_used = ', '.join(f"{tool}({count})" for tool, count in tool_summary.items())
        
        results_text.append(f"""<result rank="{i+1}">
<conversation_id>{conversation_id}</conversation_id>
<project>{project}</project>
<timestamp>{timestamp}</timestamp>
<action>{action}</action>
<tools_used>{tools_used}</tools_used>
<preview>{text_preview}</preview>
</result>""")
    
    return f"""<search_by_file>
<query>{file_path}</query>
<normalized_path>{normalized_path}</normalized_path>
<count>{len(all_results)}</count>
<results>
{''.join(results_text)}
</results>
</search_by_file>"""


@mcp.tool()
async def search_by_concept(
    ctx: Context,
    concept: str = Field(description="The concept to search for (e.g., 'security', 'docker', 'testing')"),
    include_files: bool = Field(default=True, description="Include file information in results"),
    limit: int = Field(default=10, description="Maximum number of results to return"),
    project: Optional[str] = Field(default=None, description="Search specific project only. Use 'all' to search across all projects.")
) -> str:
    """Search for conversations about a specific development concept."""
    global qdrant_client
    
    # Generate embedding for the concept
    embedding = await generate_embedding(concept)
    
    # Determine which collections to search
    # If no project specified, search all collections
    collections = await get_all_collections() if not project else []
    
    if project and project != 'all':
        # Filter collections for specific project
        normalized_project = normalize_project_name(project)
        project_hash = hashlib.md5(normalized_project.encode()).hexdigest()[:8]
        collection_prefix = f"conv_{project_hash}_"
        collections = [c for c in await get_all_collections() if c.startswith(collection_prefix)]
    elif project == 'all':
        collections = await get_all_collections()
    
    if not collections:
        return "<search_by_concept>\n<error>No collections found to search</error>\n</search_by_concept>"
    
    # First, check metadata health
    metadata_found = False
    total_points_checked = 0
    
    for collection_name in collections[:3]:  # Sample first 3 collections
        try:
            sample_points, _ = await qdrant_client.scroll(
                collection_name=collection_name,
                limit=10,
                with_payload=True
            )
            total_points_checked += len(sample_points)
            for point in sample_points:
                if 'concepts' in point.payload and point.payload['concepts']:
                    metadata_found = True
                    break
            if metadata_found:
                break
        except:
            continue
    
    # Search all collections
    all_results = []
    
    # If metadata exists, try metadata-based search first
    if metadata_found:
        for collection_name in collections:
            try:
                # Hybrid search: semantic + concept filter
                results = await qdrant_client.search(
                    collection_name=collection_name,
                    query_vector=embedding,
                    query_filter=models.Filter(
                        should=[
                            models.FieldCondition(
                                key="concepts",
                                match=models.MatchAny(any=[concept.lower()])
                            )
                        ]
                    ),
                    limit=limit * 2,  # Get more results for better filtering
                    with_payload=True
                )
                
                for point in results:
                    payload = point.payload
                    # Boost score if concept is in the concepts list
                    score_boost = 0.2 if concept.lower() in payload.get('concepts', []) else 0.0
                    all_results.append({
                        'score': float(point.score) + score_boost,
                        'payload': payload,
                        'collection': collection_name,
                        'search_type': 'metadata'
                    })
                    
            except Exception as e:
                continue
    
    # If no results from metadata search OR no metadata exists, fall back to semantic search
    if not all_results:
        await ctx.debug(f"Falling back to semantic search for concept: {concept}")
        
        for collection_name in collections:
            try:
                # Pure semantic search without filters
                results = await qdrant_client.search(
                    collection_name=collection_name,
                    query_vector=embedding,
                    limit=limit,
                    score_threshold=0.5,  # Lower threshold for broader results
                    with_payload=True
                )
                
                for point in results:
                    all_results.append({
                        'score': float(point.score),
                        'payload': point.payload,
                        'collection': collection_name,
                        'search_type': 'semantic'
                    })
                    
            except Exception as e:
                continue
    
    # Sort by score and limit
    all_results.sort(key=lambda x: x['score'], reverse=True)
    all_results = all_results[:limit]
    
    # Format results
    if not all_results:
        metadata_status = "with metadata" if metadata_found else "NO METADATA FOUND"
        return f"""<search_by_concept>
<concept>{concept}</concept>
<metadata_health>{metadata_status} (checked {total_points_checked} points)</metadata_health>
<message>No conversations found about this concept. {'Try running: python scripts/delta-metadata-update.py' if not metadata_found else 'Try different search terms.'}</message>
</search_by_concept>"""
    
    results_text = []
    for i, result in enumerate(all_results):
        payload = result['payload']
        score = result['score']
        timestamp = payload.get('timestamp', 'Unknown')
        conversation_id = payload.get('conversation_id', 'Unknown')
        project = payload.get('project', 'Unknown')
        concepts = payload.get('concepts', [])
        
        # Get text preview
        text_preview = payload.get('text', '')[:200] + '...' if len(payload.get('text', '')) > 200 else payload.get('text', '')
        
        # File information
        files_info = ""
        if include_files:
            files_analyzed = payload.get('files_analyzed', [])[:5]
            if files_analyzed:
                files_info = f"\n<files_analyzed>{', '.join(files_analyzed)}</files_analyzed>"
        
        # Related concepts
        related_concepts = [c for c in concepts if c != concept.lower()][:5]
        
        results_text.append(f"""<result rank="{i+1}">
<score>{score:.3f}</score>
<conversation_id>{conversation_id}</conversation_id>
<project>{project}</project>
<timestamp>{timestamp}</timestamp>
<concepts>{', '.join(concepts)}</concepts>
<related_concepts>{', '.join(related_concepts)}</related_concepts>{files_info}
<preview>{text_preview}</preview>
</result>""")
    
    # Determine if this was a fallback search
    used_fallback = any(r.get('search_type') == 'semantic' for r in all_results)
    metadata_status = "with metadata" if metadata_found else "NO METADATA FOUND"
    
    return f"""<search_by_concept>
<concept>{concept}</concept>
<metadata_health>{metadata_status} (checked {total_points_checked} points)</metadata_health>
<search_type>{'fallback_semantic' if used_fallback else 'metadata_based'}</search_type>
<count>{len(all_results)}</count>
<results>
{''.join(results_text)}
</results>
</search_by_concept>"""


# Debug output
print(f"[DEBUG] FastMCP server created with name: {mcp.name}")

@mcp.tool()
async def get_full_conversation(
    ctx: Context,
    conversation_id: str = Field(description="The conversation ID from search results (cid)"),
    project: Optional[str] = Field(default=None, description="Optional project name to help locate the file")
) -> str:
    """Get the full JSONL conversation file path for a conversation ID.
    This allows agents to read complete conversations instead of truncated excerpts."""
    
    # Base path for Claude conversations
    base_path = Path.home() / '.claude/projects'
    
    # Build list of directories to search
    search_dirs = []
    
    if project:
        # Try various project directory name formats
        sanitized_project = project.replace('/', '-')
        search_dirs.extend([
            base_path / project,
            base_path / sanitized_project,
            base_path / f"-Users-ramakrishnanannaswamy-projects-{project}",
            base_path / f"-Users-ramakrishnanannaswamy-projects-{sanitized_project}"
        ])
    else:
        # Search all project directories
        search_dirs = list(base_path.glob("*"))
    
    # Search for the JSONL file
    jsonl_path = None
    for search_dir in search_dirs:
        if not search_dir.is_dir():
            continue
            
        potential_path = search_dir / f"{conversation_id}.jsonl"
        if potential_path.exists():
            jsonl_path = potential_path
            break
    
    if not jsonl_path:
        # Try searching all directories as fallback
        for proj_dir in base_path.glob("*"):
            if proj_dir.is_dir():
                potential_path = proj_dir / f"{conversation_id}.jsonl"
                if potential_path.exists():
                    jsonl_path = potential_path
                    break
    
    if not jsonl_path:
        return f"""<full_conversation>
<conversation_id>{conversation_id}</conversation_id>
<status>not_found</status>
<message>Conversation file not found. Searched {len(search_dirs)} directories.</message>
<hint>Try using the project parameter or check if the conversation ID is correct.</hint>
</full_conversation>"""
    
    # Get file stats
    file_stats = jsonl_path.stat()
    
    # Count messages
    try:
        with open(jsonl_path, 'r', encoding='utf-8') as f:
            message_count = sum(1 for _ in f)
    except:
        message_count = 0
    
    return f"""<full_conversation>
<conversation_id>{conversation_id}</conversation_id>
<status>found</status>
<file_path>{jsonl_path}</file_path>
<file_size>{file_stats.st_size}</file_size>
<message_count>{message_count}</message_count>
<project>{jsonl_path.parent.name}</project>
<instructions>
You can now use the Read tool to read the full conversation from:
{jsonl_path}

Each line in the JSONL file is a separate message with complete content.
This gives you access to:
- Complete code blocks (not truncated)
- Full problem descriptions and solutions
- Entire debugging sessions
- Complete architectural decisions and discussions
</instructions>
</full_conversation>"""


# Run the server
if __name__ == "__main__":
    import sys
    
    # Handle --status command
    if len(sys.argv) > 1 and sys.argv[1] == "--status":
        import asyncio
        
        async def print_status():
            await update_indexing_status()
            # Convert timestamp to string for JSON serialization
            status_copy = indexing_status.copy()
            if status_copy["last_check"]:
                from datetime import datetime
                status_copy["last_check"] = datetime.fromtimestamp(status_copy["last_check"]).isoformat()
            else:
                status_copy["last_check"] = None
            print(json.dumps(status_copy, indent=2))
        
        asyncio.run(print_status())
        sys.exit(0)
    
    # Normal MCP server operation
    print(f"[STARTUP] Starting FastMCP server in stdio mode...", file=sys.stderr)
    print(f"[STARTUP] Server name: {mcp.name}", file=sys.stderr)
    print(f"[STARTUP] Calling mcp.run()...", file=sys.stderr)
    mcp.run()
    print(f"[STARTUP] Server exited normally", file=sys.stderr)
