"""Claude Reflect MCP Server with Memory Decay."""

import os
import asyncio
from pathlib import Path
from typing import Any, Optional, List, Dict, Union
from datetime import datetime, timezone, timedelta
import json
import hashlib
import time
import logging
import math
from xml.sax.saxutils import escape
from collections import defaultdict, Counter
import aiofiles

from fastmcp import FastMCP, Context

# Import from shared module for consistent normalization
import sys
sys.path.insert(0, str(Path(__file__).parent.parent.parent))
try:
    from shared.normalization import normalize_project_name
except ImportError:
    # Fall back to local utils if shared module not found
    from .utils import normalize_project_name
    import logging
    logging.warning("Using legacy utils.normalize_project_name - shared module not found")

from .project_resolver import ProjectResolver
from .temporal_utils import SessionDetector, TemporalParser, WorkSession, group_by_time_period
from .temporal_tools import register_temporal_tools
from .search_tools import register_search_tools
from .reflection_tools import register_reflection_tools
from .mode_switch_tool import register_mode_switch_tool
from .code_reload_tool import register_code_reload_tool
from pydantic import BaseModel, Field
from qdrant_client import AsyncQdrantClient, models
from qdrant_client.models import (
    PointStruct, VectorParams, Distance, OrderBy
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

# Setup logger early to avoid NameError
logger = logging.getLogger(__name__)
DECAY_SCALE_DAYS = float(os.getenv('DECAY_SCALE_DAYS', '90'))
USE_NATIVE_DECAY = os.getenv('USE_NATIVE_DECAY', 'false').lower() == 'true'

# Embedding configuration - now using lazy initialization
# CRITICAL: Default changed to 'true' for local embeddings for privacy
PREFER_LOCAL_EMBEDDINGS = os.getenv('PREFER_LOCAL_EMBEDDINGS', 'true').lower() == 'true'
EMBEDDING_MODEL = os.getenv('EMBEDDING_MODEL', 'sentence-transformers/all-MiniLM-L6-v2')

# Import the robust embedding manager
from .embedding_manager import get_embedding_manager

# Import new performance modules with fallback
try:
    from .connection_pool import QdrantConnectionPool, CircuitBreaker
    CONNECTION_POOL_AVAILABLE = True
except ImportError:
    CONNECTION_POOL_AVAILABLE = False
    logging.warning("Connection pool module not available")

try:
    from .parallel_search import parallel_search_collections
    PARALLEL_SEARCH_AVAILABLE = True
except ImportError:
    PARALLEL_SEARCH_AVAILABLE = False
    logging.warning("Parallel search module not available")

# Set default configuration values
MAX_RESULTS_PER_COLLECTION = 10
MAX_TOTAL_RESULTS = 1000
MAX_MEMORY_MB = 500
POOL_SIZE = 10
POOL_MAX_OVERFLOW = 5
POOL_TIMEOUT = 30.0
RETRY_ATTEMPTS = 3
RETRY_DELAY = 1.0
MAX_CONCURRENT_SEARCHES = 10
ENABLE_PARALLEL_SEARCH = True

try:
    from .decay_manager import DecayManager
    DECAY_MANAGER_AVAILABLE = True
except ImportError:
    DECAY_MANAGER_AVAILABLE = False
    logging.warning("Decay manager module not available")

class EmbeddingState:
    """Manages embedding state without global variables."""
    def __init__(self):
        self.embedding_manager = None
        self.voyage_client = None  # Keep for backward compatibility
        self.local_embedding_model = None  # Keep for backward compatibility
        self._initialized = False

    def initialize_embeddings(self):
        """Initialize embedding models with robust fallback."""
        if self._initialized:
            return True

        try:
            self.embedding_manager = get_embedding_manager()
            logger.info(f"Embedding manager initialized: {self.embedding_manager.get_model_info()}")

            # Set backward compatibility references
            if self.embedding_manager.model_type == 'voyage':
                self.voyage_client = self.embedding_manager.voyage_client
            elif self.embedding_manager.model_type == 'local':
                self.local_embedding_model = self.embedding_manager.local_model

            self._initialized = True
            return True
        except Exception as e:
            logger.error(f"Failed to initialize embeddings: {e}")
            return False

class IndexingState:
    """Manages indexing status without global variables."""
    def __init__(self):
        self.status = {
            "last_check": 0,
            "indexed_conversations": 0,
            "total_conversations": 0,
            "percentage": 100.0,
            "backlog_count": 0,
            "is_checking": False
        }
        self.cache = {"result": None, "timestamp": 0}

# Initialize state managers
embedding_state = EmbeddingState()
indexing_state = IndexingState()

# Initialize connection pool
qdrant_pool = None
circuit_breaker = None

def initialize_embeddings():
    """Initialize embedding models with robust fallback."""
    return embedding_state.initialize_embeddings()

# Debug environment loading and startup
# Debug environment loading and startup
startup_time = datetime.now(timezone.utc).isoformat()
logger.info(f"MCP Server starting at {startup_time}")
logger.info(f"Python: {sys.version}")
logger.info(f"Working directory: {os.getcwd()}")
logger.info(f"Script location: {__file__}")
logger.debug("Environment variables loaded:")
logger.debug(f"QDRANT_URL: {QDRANT_URL}")
logger.debug(f"ENABLE_MEMORY_DECAY: {ENABLE_MEMORY_DECAY}")
logger.debug(f"USE_NATIVE_DECAY: {USE_NATIVE_DECAY}")
logger.debug(f"DECAY_WEIGHT: {DECAY_WEIGHT}")
logger.debug(f"DECAY_SCALE_DAYS: {DECAY_SCALE_DAYS}")
logger.debug(f"PREFER_LOCAL_EMBEDDINGS: {PREFER_LOCAL_EMBEDDINGS}")
logger.debug(f"EMBEDDING_MODEL: {EMBEDDING_MODEL}")
logger.debug(f"env_path: {env_path}")


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

# Initialize Qdrant client with connection pooling if available
if CONNECTION_POOL_AVAILABLE and ENABLE_PARALLEL_SEARCH:
    qdrant_pool = QdrantConnectionPool(
        url=QDRANT_URL,
        pool_size=POOL_SIZE,
        max_overflow=POOL_MAX_OVERFLOW,
        timeout=POOL_TIMEOUT,
        retry_attempts=RETRY_ATTEMPTS,
        retry_delay=RETRY_DELAY
    )
    # Create a wrapper for backward compatibility
    qdrant_client = AsyncQdrantClient(url=QDRANT_URL)
    circuit_breaker = CircuitBreaker(failure_threshold=5, recovery_timeout=60.0)
    logger.info(f"Connection pool initialized with size {POOL_SIZE}")
else:
    # Fallback to single client
    qdrant_client = AsyncQdrantClient(url=QDRANT_URL)
    qdrant_pool = None
    circuit_breaker = None
    logger.info("Using single Qdrant client (no pooling)")

# Initialize decay manager if available
decay_manager = None
if DECAY_MANAGER_AVAILABLE:
    decay_manager = DecayManager()
    logger.info("Decay manager initialized")

# Add MCP Resources for system status
@mcp.resource("status://import-stats")
async def get_import_stats():
    """Current import statistics and progress."""
    await update_indexing_status()
    
    return json.dumps({
        "indexed_conversations": indexing_status["indexed_conversations"],
        "total_conversations": indexing_status["total_conversations"],
        "percentage": indexing_status["percentage"],
        "backlog_count": indexing_status["backlog_count"],
        "last_check": datetime.fromtimestamp(indexing_status["last_check"]).isoformat() if indexing_status["last_check"] else None
    }, indent=2)

@mcp.resource("status://collection-list")
async def get_collection_list():
    """List of all Qdrant collections with metadata."""
    try:
        collections = await qdrant_client.get_collections()
        collection_data = []
        
        for collection in collections.collections:
            # Get collection info
            info = await qdrant_client.get_collection(collection_name=collection.name)
            collection_data.append({
                "name": collection.name,
                "points_count": info.points_count,
                "indexed_vectors_count": info.indexed_vectors_count,
                "status": info.status,
                "config": {
                    "vector_size": info.config.params.vectors.size if hasattr(info.config.params.vectors, 'size') else 384,
                    "distance": str(info.config.params.vectors.distance) if hasattr(info.config.params.vectors, 'distance') else "Cosine"
                }
            })
        
        return json.dumps({
            "total_collections": len(collection_data),
            "collections": collection_data
        }, indent=2)
    except Exception as e:
        return json.dumps({"error": str(e)}, indent=2)

@mcp.resource("status://system-health")
async def get_system_health():
    """System health and configuration information."""
    try:
        # Check Qdrant connectivity
        qdrant_info = await qdrant_client.get_collections()
        qdrant_healthy = True
        qdrant_version = "Connected"
    except:
        qdrant_healthy = False
        qdrant_version = "Disconnected"
    
    # Check embedding configuration
    embedding_info = {}
    if embedding_state.embedding_manager:
        embedding_info = {
            "model_type": embedding_state.embedding_manager.model_type,
            "model_name": embedding_state.embedding_manager.model_name,
            "dimension": embedding_state.embedding_manager.dimension
        }
    
    return json.dumps({
        "timestamp": datetime.now(timezone.utc).isoformat(),
        "qdrant": {
            "healthy": qdrant_healthy,
            "url": QDRANT_URL,
            "version": qdrant_version
        },
        "embeddings": embedding_info,
        "configuration": {
            "memory_decay_enabled": ENABLE_MEMORY_DECAY,
            "decay_weight": DECAY_WEIGHT,
            "decay_scale_days": DECAY_SCALE_DAYS,
            "prefer_local_embeddings": PREFER_LOCAL_EMBEDDINGS
        },
        "indexing_status": {
            "indexed": indexing_status["indexed_conversations"],
            "total": indexing_status["total_conversations"],
            "percentage": indexing_status["percentage"]
        }
    }, indent=2)

# Legacy support for old variable names
indexing_status = indexing_state.status
_indexing_cache = indexing_state.cache

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


async def read_json_file(path: Path) -> dict:
    """Read JSON file from disk."""
    async with aiofiles.open(path, 'r') as f:
        content = await f.read()
        return json.loads(content)


async def read_watcher_file(path: Path) -> dict:
    """Read watcher JSON file from disk."""
    async with aiofiles.open(path, 'r') as f:
        content = await f.read()
        return json.loads(content)


async def read_cloud_file(path: Path) -> dict:
    """Read cloud watcher JSON file from disk."""
    async with aiofiles.open(path, 'r') as f:
        content = await f.read()
        return json.loads(content)


async def _load_state_files() -> tuple[set[str], dict[str, dict]]:
    """Load and merge all state files to get imported file tracking."""
    all_imported_files = set()
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
                imported_data = await read_json_file(path)
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
                watcher_data = await read_watcher_file(path)
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
            cloud_data = await read_cloud_file(cloud_watcher_path)
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

    return all_imported_files, file_metadata


def _is_file_imported(file_path: Path, imported_files_list: list[str], file_metadata: dict[str, dict]) -> bool:
    """Check if a file has been imported using multiple path matching strategies."""
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
    if any(path in imported_files_list for path in [normalized_file, normalized_alt, normalized_alt2]):
        return True

    # Or if it has metadata with position > 0 (partially imported)
    return any(
        path in file_metadata and file_metadata[path].get("position", 0) > 0
        for path in [normalized_file, normalized_alt, normalized_alt2]
    )


def _should_skip_indexing_check(cache: dict, status: dict, current_time: float, cache_ttl: int) -> bool:
    """Check if indexing status update should be skipped due to cache or rate limiting."""
    # Check cache first (5-second TTL to prevent performance issues)
    if cache["result"] and current_time - cache["timestamp"] < cache_ttl:
        # Use cached result
        status.update(cache["result"])
        return True

    # Don't run concurrent checks
    if status["is_checking"]:
        return True

    # Check immediately on first call, then every 60 seconds to avoid overhead
    if status["last_check"] > 0 and current_time - status["last_check"] < 60:  # 1 minute
        return True

    return False


async def _count_indexed_files() -> tuple[int, int]:
    """Count total JSONL files and how many have been indexed."""
    projects_dir = Path.home() / ".claude" / "projects"
    total_files = 0
    indexed_files = 0

    if projects_dir.exists():
        # Get all JSONL files
        jsonl_files = list(projects_dir.glob("**/*.jsonl"))
        total_files = len(jsonl_files)

        # Load state from all tracking files
        all_imported_files, file_metadata = await _load_state_files()
        imported_files_list = list(all_imported_files)

        # Count files that have been imported
        for file_path in jsonl_files:
            if _is_file_imported(file_path, imported_files_list, file_metadata):
                indexed_files += 1

    return total_files, indexed_files


def _update_status_metrics(status: dict, cache: dict, current_time: float,
                          total_files: int, indexed_files: int) -> None:
    """Update the status metrics and cache."""
    # Update status
    status["last_check"] = current_time
    status["total_conversations"] = total_files
    status["indexed_conversations"] = indexed_files
    status["backlog_count"] = total_files - indexed_files

    if total_files > 0:
        status["percentage"] = (indexed_files / total_files) * 100
    else:
        status["percentage"] = 100.0

    # Update cache
    cache["result"] = status.copy()
    cache["timestamp"] = current_time


async def update_indexing_status(cache_ttl: int = 5):
    """Update indexing status by checking JSONL files vs Qdrant collections.
    This is a lightweight check that compares file counts, not full content.

    Args:
        cache_ttl: Cache time-to-live in seconds (default: 5)
    """
    status = indexing_state.status
    cache = indexing_state.cache
    current_time = time.time()

    # Check if we should skip this update
    if _should_skip_indexing_check(cache, status, current_time, cache_ttl):
        return

    status["is_checking"] = True

    try:
        # Count files and their indexing status
        total_files, indexed_files = await _count_indexed_files()

        # Update all metrics
        _update_status_metrics(status, cache, current_time, total_files, indexed_files)

    except Exception as e:
        logger.warning(f"Failed to update indexing status: {e}")
        logger.error(f"Failed to update indexing status: {e}", exc_info=True)
    finally:
        status["is_checking"] = False
    
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
    # Initialize on first use
    if embedding_state.embedding_manager is None:
        if not embedding_state.initialize_embeddings():
            raise RuntimeError("Failed to initialize any embedding model. Check logs for details.")

    # Determine which type to use
    if force_type:
        use_local = force_type == 'local'
    else:
        use_local = embedding_state.embedding_manager.model_type == 'local'

    if use_local:
        # Use local embeddings
        if not embedding_state.local_embedding_model:
            raise ValueError("Local embedding model not available")

        # Run in executor since fastembed is synchronous
        loop = asyncio.get_event_loop()
        embeddings = await loop.run_in_executor(
            None, lambda: list(embedding_state.local_embedding_model.embed([text]))
        )
        return embeddings[0].tolist()
    else:
        # Use Voyage AI
        if not embedding_state.voyage_client:
            raise ValueError("Voyage client not available")
        result = embedding_state.voyage_client.embed(
            texts=[text],
            model="voyage-3-large",
            input_type="query"
        )
        return result.embeddings[0]

def get_embedding_dimension() -> int:
    """Get the dimension of embeddings based on the provider."""
    if PREFER_LOCAL_EMBEDDINGS or not embedding_state.voyage_client:
        # all-MiniLM-L6-v2 produces 384-dimensional embeddings
        return 384
    else:
        # voyage-3-large produces 1024-dimensional embeddings
        return 1024

def get_collection_suffix() -> str:
    """Get the collection suffix based on embedding provider."""
    # Use embedding_manager's model type if available
    if embedding_state.embedding_manager and hasattr(embedding_state.embedding_manager, 'model_type'):
        if embedding_state.embedding_manager.model_type == 'voyage':
            return "_voyage"
        else:
            return "_local"
    # Fallback to environment variable
    elif PREFER_LOCAL_EMBEDDINGS:
        return "_local"
    else:
        return "_voyage"

def aggregate_pattern_intelligence(results: List[SearchResult]) -> Dict[str, Any]:
    """Aggregate pattern intelligence across search results."""
    
    # Initialize counters using efficient data structures
    all_patterns = Counter()
    all_files = set()
    all_tools = set()
    all_concepts = set()
    pattern_by_category = defaultdict(lambda: defaultdict(int))

    for result in results:
        # Aggregate code patterns efficiently using Counter operations
        if result.code_patterns:
            for category, patterns in result.code_patterns.items():
                # Use Counter for efficient bulk updates
                pattern_counter = Counter(patterns)
                pattern_by_category[category].update(pattern_counter)
                all_patterns.update(pattern_counter)
        
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
        "pattern_by_category": {k: dict(v) for k, v in pattern_by_category.items()},
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

# Register temporal tools after all functions are defined
register_temporal_tools(
    mcp,
    qdrant_client,
    QDRANT_URL,
    get_all_collections,
    generate_embedding,
    initialize_embeddings,
    normalize_project_name
)
logger.info("Temporal tools registered")

# Register search tools
def get_embedding_manager():
    """Factory function to get the current embedding manager."""
    from .embedding_manager import get_embedding_manager as get_em
    return get_em()

# Initialize ProjectResolver for collection name mapping
# ProjectResolver needs a sync client, not async
from qdrant_client import QdrantClient as SyncQdrantClient
sync_qdrant_client = SyncQdrantClient(url=QDRANT_URL)
project_resolver = ProjectResolver(sync_qdrant_client)

register_search_tools(
    mcp,
    qdrant_client,
    QDRANT_URL,
    get_embedding_manager,
    normalize_project_name,
    ENABLE_MEMORY_DECAY,
    DECAY_WEIGHT,
    DECAY_SCALE_DAYS,
    USE_NATIVE_DECAY,
    NATIVE_DECAY_AVAILABLE,
    decay_manager,
    project_resolver  # Pass the resolver
)

# Register reflection tools
register_reflection_tools(
    mcp,
    qdrant_client,
    QDRANT_URL,
    get_embedding_manager,
    normalize_project_name
)

# Register mode switching tools
register_mode_switch_tool(
    mcp,
    get_embedding_manager
)

# Register code reload tools
register_code_reload_tool(
    mcp,
    get_embedding_manager
)

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
            logger.info(json.dumps(status_copy, indent=2))
        
        asyncio.run(print_status())
        sys.exit(0)
    
    # Normal MCP server operation
    logger.info("Starting FastMCP server in stdio mode...")
    logger.info(f"Server name: {mcp.name}")
    logger.info("Calling mcp.run()...")
    mcp.run()
    logger.info("Server exited normally")
