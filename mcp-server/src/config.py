"""Configuration and environment constants for Claude Self-Reflect MCP server."""

import os
from pathlib import Path
from dotenv import load_dotenv

# Load environment variables
load_dotenv()

# API Keys
VOYAGE_API_KEY = os.getenv('VOYAGE_API_KEY', '')
QDRANT_URL = os.getenv('QDRANT_URL', 'http://localhost:6333')

# Embedding Configuration
PREFER_LOCAL_EMBEDDINGS = os.getenv('PREFER_LOCAL_EMBEDDINGS', 'true').lower() == 'true'
VOYAGE_MODEL = "voyage-3-lite"
LOCAL_MODEL = "sentence-transformers/all-MiniLM-L6-v2"

# Decay Configuration
USE_DECAY = os.getenv('USE_DECAY', 'false').lower() == 'true'
DECAY_SCALE_DAYS = int(os.getenv('DECAY_SCALE_DAYS', '90'))
DECAY_WEIGHT = float(os.getenv('DECAY_WEIGHT', '0.3'))
USE_NATIVE_DECAY = os.getenv('USE_NATIVE_DECAY', 'false').lower() == 'true'

# Search Configuration
DEFAULT_SEARCH_LIMIT = 5
MAX_SEARCH_LIMIT = 100
DEFAULT_MIN_SCORE = 0.3

# Memory Management
MAX_RESULTS_PER_COLLECTION = int(os.getenv('MAX_RESULTS_PER_COLLECTION', '10'))
MAX_TOTAL_RESULTS = int(os.getenv('MAX_TOTAL_RESULTS', '1000'))
MAX_MEMORY_MB = int(os.getenv('MAX_MEMORY_MB', '500'))

# Connection Pool Configuration
POOL_SIZE = int(os.getenv('QDRANT_POOL_SIZE', '10'))
POOL_MAX_OVERFLOW = int(os.getenv('QDRANT_POOL_OVERFLOW', '5'))
POOL_TIMEOUT = float(os.getenv('QDRANT_POOL_TIMEOUT', '30.0'))
RETRY_ATTEMPTS = int(os.getenv('QDRANT_RETRY_ATTEMPTS', '3'))
RETRY_DELAY = float(os.getenv('QDRANT_RETRY_DELAY', '1.0'))

# Performance Configuration
MAX_CONCURRENT_SEARCHES = int(os.getenv('MAX_CONCURRENT_SEARCHES', '10'))
ENABLE_PARALLEL_SEARCH = os.getenv('ENABLE_PARALLEL_SEARCH', 'true').lower() == 'true'

# Paths
CLAUDE_PROJECTS_PATH = Path.home() / '.claude' / 'projects'
CONFIG_PATH = Path.home() / '.claude-self-reflect' / 'config'

# Collection Naming
VOYAGE_SUFFIX = '_voyage'
LOCAL_SUFFIX = '_local'

# Logging
import logging
logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)