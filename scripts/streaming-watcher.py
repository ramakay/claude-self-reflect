#!/usr/bin/env python3
"""
Claude Self-Reflect Production Streaming Watcher v3.0.0
Complete overhaul with all fixes from v2.5.17 plus enhanced monitoring

Key improvements:
1. Production state file: csr-watcher.json (no temp/test names)
2. Comprehensive psutil memory monitoring with detailed metrics
3. Proper state key format handling (full paths)
4. Container-aware configuration for Docker deployments
5. Enhanced error recovery and queue management
6. Real-time progress tracking toward 100% indexing
"""

import asyncio
import json
import os
import time
import hashlib
import re
import gc
import ctypes
import platform
from pathlib import Path
from typing import Dict, List, Optional, Any, Set, Tuple, Generator
from datetime import datetime, timedelta
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from enum import Enum
import logging
from collections import deque

from qdrant_client import AsyncQdrantClient, models
from qdrant_client.http.exceptions import UnexpectedResponse
from fastembed import TextEmbedding
import psutil

# Import normalize_project_name
import sys
sys.path.insert(0, str(Path(__file__).parent))
from utils import normalize_project_name

# Configure logging
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

# Configuration from environment
@dataclass
class Config:
    """Production configuration with proper defaults."""
    qdrant_url: str = field(default_factory=lambda: os.getenv("QDRANT_URL", "http://localhost:6333"))
    voyage_api_key: Optional[str] = field(default_factory=lambda: os.getenv("VOYAGE_API_KEY"))
    prefer_local_embeddings: bool = field(default_factory=lambda: os.getenv("PREFER_LOCAL_EMBEDDINGS", "true").lower() == "true")
    embedding_model: str = field(default_factory=lambda: os.getenv("EMBEDDING_MODEL", "sentence-transformers/all-MiniLM-L6-v2"))
    
    logs_dir: Path = field(default_factory=lambda: Path(os.getenv("LOGS_DIR", "~/.claude/projects")).expanduser())
    
    # Production state file with proper naming
    state_file: Path = field(default_factory=lambda: (
        # Docker/cloud mode: use /config volume
        Path("/config/csr-watcher.json") if os.path.exists("/.dockerenv") 
        # Local mode with cloud flag: separate state file
        else Path("~/.claude-self-reflect/config/csr-watcher-cloud.json").expanduser() 
        if os.getenv("PREFER_LOCAL_EMBEDDINGS", "true").lower() == "false" and os.getenv("VOYAGE_API_KEY")
        # Default local mode
        else Path("~/.claude-self-reflect/config/csr-watcher.json").expanduser()
        if os.getenv("STATE_FILE") is None
        # User override
        else Path(os.getenv("STATE_FILE")).expanduser()
    ))
    
    collection_prefix: str = "conv"
    vector_size: int = 384  # FastEmbed all-MiniLM-L6-v2
    
    # Production throttling controls (optimized for stability)
    import_frequency: int = field(default_factory=lambda: int(os.getenv("IMPORT_FREQUENCY", "60")))  # Normal cycle
    hot_check_interval_s: int = field(default_factory=lambda: int(os.getenv("HOT_CHECK_INTERVAL_S", "2")))  # HOT file check
    batch_size: int = field(default_factory=lambda: int(os.getenv("BATCH_SIZE", "10")))
    memory_limit_mb: int = field(default_factory=lambda: int(os.getenv("MEMORY_LIMIT_MB", "1024")))  # 1GB default
    memory_warning_mb: int = field(default_factory=lambda: int(os.getenv("MEMORY_WARNING_MB", "500")))  # 500MB warning
    
    # HOT/WARM/COLD configuration
    hot_window_minutes: int = field(default_factory=lambda: int(os.getenv("HOT_WINDOW_MINUTES", "5")))  # Files < 5 min are HOT
    warm_window_hours: int = field(default_factory=lambda: int(os.getenv("WARM_WINDOW_HOURS", "24")))  # Files < 24 hrs are WARM
    max_cold_files: int = field(default_factory=lambda: int(os.getenv("MAX_COLD_FILES", "5")))  # Max COLD files per cycle
    max_warm_wait_minutes: int = field(default_factory=lambda: int(os.getenv("MAX_WARM_WAIT_MINUTES", "30")))  # Starvation prevention
    
    # CPU management
    max_cpu_percent_per_core: float = field(default_factory=lambda: float(os.getenv("MAX_CPU_PERCENT_PER_CORE", "50")))
    max_concurrent_embeddings: int = field(default_factory=lambda: int(os.getenv("MAX_CONCURRENT_EMBEDDINGS", "2")))
    max_concurrent_qdrant: int = field(default_factory=lambda: int(os.getenv("MAX_CONCURRENT_QDRANT", "3")))
    
    # Queue management
    max_queue_size: int = field(default_factory=lambda: int(os.getenv("MAX_QUEUE_SIZE", "100")))
    max_backlog_hours: int = field(default_factory=lambda: int(os.getenv("MAX_BACKLOG_HOURS", "24")))
    
    # Reliability settings
    qdrant_timeout_s: float = field(default_factory=lambda: float(os.getenv("QDRANT_TIMEOUT", "10")))
    max_retries: int = field(default_factory=lambda: int(os.getenv("MAX_RETRIES", "3")))
    retry_delay_s: float = field(default_factory=lambda: float(os.getenv("RETRY_DELAY", "1")))
    
    # Collection cache settings
    collection_cache_ttl: int = field(default_factory=lambda: int(os.getenv("COLLECTION_CACHE_TTL", "3600")))
    collection_cache_max_size: int = field(default_factory=lambda: int(os.getenv("COLLECTION_CACHE_MAX_SIZE", "100")))


# Check if malloc_trim is available
try:
    libc = ctypes.CDLL("libc.so.6")
    malloc_trim = libc.malloc_trim
    malloc_trim.argtypes = [ctypes.c_size_t]
    malloc_trim.restype = ctypes.c_int
    MALLOC_TRIM_AVAILABLE = True
except:
    MALLOC_TRIM_AVAILABLE = False
    logger.debug("malloc_trim not available on this platform")


def get_effective_cpus() -> float:
    """Get effective CPU count considering cgroup limits."""
    effective_cores_env = os.getenv("EFFECTIVE_CORES")
    if effective_cores_env:
        try:
            return float(effective_cores_env)
        except ValueError:
            pass
    
    # cgroup v2
    cpu_max = Path("/sys/fs/cgroup/cpu.max")
    # cgroup v1
    cpu_quota = Path("/sys/fs/cgroup/cpu/cpu.cfs_quota_us")
    cpu_period = Path("/sys/fs/cgroup/cpu/cpu.cfs_period_us")
    
    try:
        if cpu_max.exists():
            content = cpu_max.read_text().strip().split()
            if content[0] != "max":
                quota, period = int(content[0]), int(content[1])
                if period > 0:
                    return max(1.0, quota / period)
        elif cpu_quota.exists() and cpu_period.exists():
            quota = int(cpu_quota.read_text())
            period = int(cpu_period.read_text())
            if quota > 0 and period > 0:
                return max(1.0, quota / period)
    except Exception:
        pass
    
    return float(psutil.cpu_count() or 1)


def extract_tool_usage_from_conversation(messages: List[Dict]) -> Dict[str, Any]:
    """Extract tool usage metadata from conversation messages."""
    tool_usage = {
        'files_analyzed': [],
        'files_edited': [],
        'tools_used': set()
    }
    
    for msg in messages:
        content = msg.get('content', '')
        
        if isinstance(content, str):
            text = content
        elif isinstance(content, list):
            text_parts = []
            for item in content:
                if isinstance(item, str):
                    text_parts.append(item)
                elif isinstance(item, dict):
                    if item.get('type') == 'text':
                        text_parts.append(item.get('text', ''))
                    elif item.get('type') == 'tool_use':
                        tool_name = item.get('name', '')
                        tool_usage['tools_used'].add(tool_name)
                        
                        if 'input' in item:
                            tool_input = item['input']
                            if isinstance(tool_input, dict):
                                if 'file_path' in tool_input:
                                    file_path = tool_input['file_path']
                                    if tool_name in ['Read', 'Grep', 'Glob', 'LS']:
                                        tool_usage['files_analyzed'].append(file_path)
                                    elif tool_name in ['Edit', 'Write', 'MultiEdit']:
                                        tool_usage['files_edited'].append(file_path)
                                
                                if 'files' in tool_input:
                                    files = tool_input['files']
                                    if isinstance(files, list):
                                        tool_usage['files_analyzed'].extend(files)
            text = ' '.join(text_parts)
        else:
            text = str(content) if content else ''
        
        # Extract file paths from text content
        file_patterns = [
            r'`([/\w\-\.]+\.\w+)`',
            r'File: ([/\w\-\.]+\.\w+)',
            r'(?:^|\s)(/[\w\-\./]+\.\w+)',
            r'(?:^|\s)([\w\-]+\.\w+)',
        ]
        
        for pattern in file_patterns:
            matches = re.findall(pattern, text[:5000])
            for match in matches[:10]:
                if match and not match.startswith('http'):
                    if any(keyword in text.lower() for keyword in ['edit', 'modify', 'update', 'write', 'create']):
                        tool_usage['files_edited'].append(match)
                    else:
                        tool_usage['files_analyzed'].append(match)
    
    # Convert sets to lists and deduplicate
    tool_usage['tools_used'] = list(tool_usage['tools_used'])
    tool_usage['files_analyzed'] = list(set(tool_usage['files_analyzed']))[:20]
    tool_usage['files_edited'] = list(set(tool_usage['files_edited']))[:20]
    
    return tool_usage


def extract_concepts(text: str, tool_usage: Dict[str, Any]) -> List[str]:
    """Extract development concepts from conversation text."""
    concepts = set()
    
    text_sample = text[:50000] if len(text) > 50000 else text
    
    concept_patterns = {
        'docker': r'\b(?:docker|container|compose|dockerfile)\b',
        'testing': r'\b(?:test|testing|unittest|pytest)\b',
        'database': r'\b(?:database|sql|postgres|mysql|mongodb|qdrant)\b',
        'api': r'\b(?:api|rest|graphql|endpoint|mcp)\b',
        'security': r'\b(?:security|auth|authentication)\b',
        'performance': r'\b(?:performance|optimization|cache|memory)\b',
        'debugging': r'\b(?:debug|debugging|error|bug|fix)\b',
        'deployment': r'\b(?:deploy|deployment|ci\/cd)\b',
        'streaming': r'\b(?:stream|streaming|import|watcher)\b',
        'embeddings': r'\b(?:embed|embedding|vector|fastembed|voyage)\b',
    }
    
    text_lower = text_sample.lower()
    
    for concept, pattern in concept_patterns.items():
        if re.search(pattern, text_lower, re.IGNORECASE):
            concepts.add(concept)
    
    # Add concepts based on tools used
    if 'Docker' in tool_usage.get('tools_used', []):
        concepts.add('docker')
    if 'Bash' in tool_usage.get('tools_used', []):
        concepts.add('scripting')
    
    return list(concepts)[:15]


class FreshnessLevel(Enum):
    """File freshness categorization for prioritization."""
    HOT = "HOT"           # < 5 minutes old - near real-time processing
    WARM = "WARM"         # 5 minutes - 24 hours - normal processing
    COLD = "COLD"         # > 24 hours - batch processing
    URGENT_WARM = "URGENT_WARM"  # WARM files waiting > 30 minutes (starvation prevention)


class MemoryMonitor:
    """Enhanced memory monitoring with psutil."""
    
    def __init__(self, limit_mb: int, warning_mb: int):
        self.process = psutil.Process()
        self.limit_mb = limit_mb
        self.warning_mb = warning_mb
        self.start_memory = self.get_memory_info()
        self.peak_memory = self.start_memory['rss_mb']
        self.cleanup_count = 0
        self.last_warning_time = 0
        
    def get_memory_info(self) -> Dict[str, float]:
        """Get detailed memory information."""
        mem = self.process.memory_info()
        
        # Get additional memory metrics
        try:
            mem_full = self.process.memory_full_info()
            uss = mem_full.uss / 1024 / 1024  # Unique set size
            pss = mem_full.pss / 1024 / 1024 if hasattr(mem_full, 'pss') else 0  # Proportional set size
        except:
            uss = 0
            pss = 0
        
        return {
            'rss_mb': mem.rss / 1024 / 1024,  # Resident set size
            'vms_mb': mem.vms / 1024 / 1024,  # Virtual memory size
            'uss_mb': uss,  # Unique memory
            'pss_mb': pss,  # Proportional memory
            'percent': self.process.memory_percent(),
            'available_mb': psutil.virtual_memory().available / 1024 / 1024
        }
    
    def check_memory(self) -> Tuple[bool, Dict[str, Any]]:
        """Check memory usage and return (should_cleanup, metrics)."""
        info = self.get_memory_info()
        rss_mb = info['rss_mb']
        
        # Update peak
        self.peak_memory = max(self.peak_memory, rss_mb)
        
        # Check thresholds
        should_cleanup = False
        alert_level = "normal"
        
        if rss_mb > self.limit_mb:
            alert_level = "critical"
            should_cleanup = True
        elif rss_mb > self.limit_mb * 0.85:
            alert_level = "high"
            should_cleanup = True
        elif rss_mb > self.warning_mb:
            alert_level = "warning"
            # Only warn once per minute
            now = time.time()
            if now - self.last_warning_time > 60:
                logger.warning(f"Memory usage {rss_mb:.1f}MB exceeds warning threshold {self.warning_mb}MB")
                self.last_warning_time = now
        
        return should_cleanup, {
            'current_mb': rss_mb,
            'peak_mb': self.peak_memory,
            'limit_mb': self.limit_mb,
            'warning_mb': self.warning_mb,
            'percent_of_limit': (rss_mb / self.limit_mb * 100) if self.limit_mb > 0 else 0,
            'alert_level': alert_level,
            'cleanup_count': self.cleanup_count,
            'details': info
        }
    
    async def cleanup(self) -> Dict[str, Any]:
        """Perform memory cleanup and return metrics."""
        before = self.get_memory_info()
        
        # Force garbage collection
        gc.collect(2)  # Full collection
        
        # Platform-specific cleanup
        if MALLOC_TRIM_AVAILABLE:
            malloc_trim(0)
        
        # Give system time to reclaim
        await asyncio.sleep(0.1)
        
        after = self.get_memory_info()
        self.cleanup_count += 1
        
        freed = before['rss_mb'] - after['rss_mb']
        
        if freed > 10:  # Significant cleanup
            logger.info(f"Memory cleanup freed {freed:.1f}MB (before: {before['rss_mb']:.1f}MB, after: {after['rss_mb']:.1f}MB)")
        
        return {
            'before_mb': before['rss_mb'],
            'after_mb': after['rss_mb'],
            'freed_mb': freed,
            'cleanup_count': self.cleanup_count
        }


class EmbeddingProvider:
    """Base class for embedding providers."""
    
    async def embed_documents(self, texts: List[str]) -> List[List[float]]:
        raise NotImplementedError
    
    async def close(self):
        """Cleanup resources."""
        pass


class FastEmbedProvider(EmbeddingProvider):
    """FastEmbed provider with proper resource management."""
    
    def __init__(self, model_name: str, max_concurrent: int = 2):
        self.model = TextEmbedding(model_name)
        self.executor = ThreadPoolExecutor(max_workers=1)
        self.semaphore = asyncio.Semaphore(max_concurrent)
        self.vector_size = 384  # all-MiniLM-L6-v2 dimensions
        self.provider_type = 'local'
    
    async def embed_documents(self, texts: List[str]) -> List[List[float]]:
        """Generate embeddings with concurrency control."""
        async with self.semaphore:
            loop = asyncio.get_event_loop()
            embeddings = await loop.run_in_executor(
                self.executor,
                lambda: list(self.model.embed(texts))
            )
            return [embedding.tolist() for embedding in embeddings]
    
    async def close(self):
        """Shutdown executor properly."""
        if sys.version_info >= (3, 9):
            self.executor.shutdown(wait=True, cancel_futures=True)
        else:
            self.executor.shutdown(wait=True)


class VoyageProvider(EmbeddingProvider):
    """Voyage AI provider for cloud embeddings with retry logic."""
    
    def __init__(self, api_key: str, model_name: str = "voyage-3", max_concurrent: int = 2):
        self.api_key = api_key
        self.model_name = model_name
        self.vector_size = 1024  # voyage-3 dimension
        self.semaphore = asyncio.Semaphore(max_concurrent)
        self.base_url = "https://api.voyageai.com/v1/embeddings"
        self.session = None
        self.max_retries = 3
        self.retry_delay = 1.0
    
    async def _ensure_session(self):
        """Ensure aiohttp session exists."""
        if self.session is None:
            import aiohttp
            self.session = aiohttp.ClientSession()
    
    async def embed_documents(self, texts: List[str]) -> List[List[float]]:
        """Generate embeddings using Voyage AI API with retry logic."""
        await self._ensure_session()
        
        async with self.semaphore:
            for attempt in range(self.max_retries):
                try:
                    import aiohttp
                    headers = {
                        "Authorization": f"Bearer {self.api_key}",
                        "Content-Type": "application/json"
                    }
                    
                    payload = {
                        "input": texts,
                        "model": self.model_name,
                        "input_type": "document"  # For document embeddings
                    }
                    
                    async with self.session.post(
                        self.base_url,
                        headers=headers,
                        json=payload,
                        timeout=aiohttp.ClientTimeout(total=30)
                    ) as response:
                        if response.status == 200:
                            data = await response.json()
                            # Voyage returns embeddings in data.data[].embedding
                            embeddings = [item["embedding"] for item in data["data"]]
                            return embeddings
                        elif response.status == 429:  # Rate limit
                            retry_after = int(response.headers.get("Retry-After", 2))
                            logger.warning(f"Rate limited, retrying after {retry_after}s")
                            await asyncio.sleep(retry_after)
                        else:
                            error_text = await response.text()
                            logger.error(f"Voyage API error {response.status}: {error_text}")
                            if attempt < self.max_retries - 1:
                                await asyncio.sleep(self.retry_delay * (2 ** attempt))
                            
                except asyncio.TimeoutError:
                    logger.warning(f"Voyage API timeout (attempt {attempt + 1}/{self.max_retries})")
                    if attempt < self.max_retries - 1:
                        await asyncio.sleep(self.retry_delay * (2 ** attempt))
                except Exception as e:
                    logger.error(f"Voyage API error: {e}")
                    if attempt < self.max_retries - 1:
                        await asyncio.sleep(self.retry_delay * (2 ** attempt))
            
            raise Exception(f"Failed to get embeddings after {self.max_retries} attempts")
    
    async def close(self):
        """Close aiohttp session."""
        if self.session:
            await self.session.close()
            self.session = None


class QdrantService:
    """Qdrant service with proper backpressure and retries."""
    
    def __init__(self, config: Config, embedding_provider: EmbeddingProvider):
        self.config = config
        self.client = AsyncQdrantClient(url=config.qdrant_url)
        self.embedding_provider = embedding_provider
        self._collection_cache: Dict[str, float] = {}
        self.request_semaphore = asyncio.Semaphore(config.max_concurrent_qdrant)
    
    async def ensure_collection(self, collection_name: str) -> None:
        """Ensure collection exists with TTL cache."""
        now = time.time()
        
        if collection_name in self._collection_cache:
            if now - self._collection_cache[collection_name] < self.config.collection_cache_ttl:
                return
        
        if len(self._collection_cache) >= self.config.collection_cache_max_size:
            oldest = min(self._collection_cache.items(), key=lambda x: x[1])
            del self._collection_cache[oldest[0]]
        
        async with self.request_semaphore:
            try:
                await asyncio.wait_for(
                    self.client.get_collection(collection_name),
                    timeout=self.config.qdrant_timeout_s
                )
                self._collection_cache[collection_name] = now
                logger.debug(f"Collection {collection_name} exists")
            except (UnexpectedResponse, asyncio.TimeoutError):
                # Create collection with correct vector size based on provider
                vector_size = self.embedding_provider.vector_size or self.config.vector_size
                
                try:
                    await asyncio.wait_for(
                        self.client.create_collection(
                            collection_name=collection_name,
                            vectors_config=models.VectorParams(
                                size=vector_size,
                                distance=models.Distance.COSINE
                            ),
                            optimizers_config=models.OptimizersConfigDiff(
                                indexing_threshold=100
                            )
                        ),
                        timeout=self.config.qdrant_timeout_s
                    )
                    self._collection_cache[collection_name] = now
                    logger.info(f"Created collection {collection_name}")
                except UnexpectedResponse as e:
                    if "already exists" in str(e):
                        self._collection_cache[collection_name] = now
                    else:
                        raise
    
    async def store_points_with_retry(
        self,
        collection_name: str,
        points: List[models.PointStruct]
    ) -> bool:
        """Store points with retry logic."""
        if not points:
            return True
        
        for attempt in range(self.config.max_retries):
            try:
                async with self.request_semaphore:
                    # Directly await with timeout to avoid orphaned tasks
                    await asyncio.wait_for(
                        self.client.upsert(
                            collection_name=collection_name,
                            points=points,
                            wait=True
                        ),
                        timeout=self.config.qdrant_timeout_s
                    )
                    logger.debug(f"Stored {len(points)} points in {collection_name}")
                    return True
                    
            except asyncio.TimeoutError:
                # Don't cancel - let it complete in background to avoid race condition
                logger.warning(f"Timeout storing points (attempt {attempt + 1}/{self.config.max_retries})")
                if attempt < self.config.max_retries - 1:
                    await asyncio.sleep(self.config.retry_delay_s * (2 ** attempt))
            except Exception as e:
                logger.error(f"Error storing points: {e}")
                if attempt < self.config.max_retries - 1:
                    await asyncio.sleep(self.config.retry_delay_s)
        
        return False
    
    async def close(self):
        """Close client connection."""
        self._collection_cache.clear()
        try:
            await self.client.close()  # Close AsyncQdrantClient connections
        except AttributeError:
            pass  # Older versions might not have close()


class TokenAwareChunker:
    """Memory-efficient streaming chunker."""
    
    def __init__(self, chunk_size_tokens: int = 400, chunk_overlap_tokens: int = 75):
        self.chunk_size_chars = chunk_size_tokens * 4
        self.chunk_overlap_chars = chunk_overlap_tokens * 4
        logger.info(f"TokenAwareChunker: {chunk_size_tokens} tokens (~{self.chunk_size_chars} chars)")
    
    def chunk_text_stream(self, text: str) -> Generator[str, None, None]:
        """Stream chunks without holding all in memory."""
        if not text:
            return
        
        if len(text) <= self.chunk_size_chars:
            yield text
            return
        
        start = 0
        while start < len(text):
            end = min(start + self.chunk_size_chars, len(text))
            
            if end < len(text):
                for separator in ['. ', '.\n', '! ', '? ', '\n\n', '\n', ' ']:
                    last_sep = text.rfind(separator, start, end)
                    if last_sep > start + (self.chunk_size_chars // 2):
                        end = last_sep + len(separator)
                        break
            
            chunk = text[start:end].strip()
            if chunk:
                yield chunk
            
            if end >= len(text):
                break
            start = max(start + 1, end - self.chunk_overlap_chars)


class CPUMonitor:
    """Non-blocking CPU monitoring with cgroup awareness."""
    
    def __init__(self, max_cpu_per_core: float):
        self.process = psutil.Process()
        effective_cores = get_effective_cpus()
        self.max_total_cpu = max_cpu_per_core * effective_cores
        logger.info(f"CPU Monitor: {effective_cores:.1f} effective cores, {self.max_total_cpu:.1f}% limit")
        
        self.process.cpu_percent(interval=None)
        time.sleep(0.01)
        self.last_check = time.time()
        self.last_cpu = self.process.cpu_percent(interval=None)
    
    def get_cpu_nowait(self) -> float:
        """Get CPU without blocking."""
        now = time.time()
        if now - self.last_check > 1.0:
            val = self.process.cpu_percent(interval=None)
            if val == 0.0 and self.last_cpu == 0.0:
                time.sleep(0.01)
                val = self.process.cpu_percent(interval=None)
            self.last_cpu = val
            self.last_check = now
        return self.last_cpu
    
    def should_throttle(self) -> bool:
        """Check if we should throttle based on CPU."""
        return self.get_cpu_nowait() > self.max_total_cpu


class QueueManager:
    """Manage file processing queue with priority support and deduplication."""
    
    def __init__(self, max_size: int, max_age_hours: int):
        self.max_size = max_size
        self.max_age = timedelta(hours=max_age_hours)
        # Queue stores (path, mod_time, freshness_level, priority_score)
        self.queue: deque = deque()
        self._queued: Set[str] = set()  # Track queued files to prevent duplicates
        self.processed_count = 0
        self.deferred_count = 0
    
    def add_categorized(self, items: List[Tuple[Path, datetime, FreshnessLevel, int]]) -> int:
        """Add categorized files with priority handling."""
        added = 0
        overflow = []
        
        for file_path, mod_time, level, priority in items:
            key = str(file_path)
            
            # Skip if already queued
            if key in self._queued:
                continue
                
            if len(self.queue) >= self.max_size:
                overflow.append((file_path, mod_time))
                continue
            
            # HOT and URGENT_WARM go to front of queue
            if level in (FreshnessLevel.HOT, FreshnessLevel.URGENT_WARM):
                self.queue.appendleft((file_path, mod_time, level, priority))
            else:
                self.queue.append((file_path, mod_time, level, priority))
            
            self._queued.add(key)
            added += 1
        
        if overflow:
            self.deferred_count += len(overflow)
            oldest = min(overflow, key=lambda x: x[1])
            logger.critical(f"QUEUE OVERFLOW: {len(overflow)} files deferred. "
                          f"Oldest: {oldest[0].name} ({(datetime.now() - oldest[1]).total_seconds() / 3600:.1f}h old)")
        
        return added
    
    def get_batch(self, batch_size: int) -> List[Tuple[Path, FreshnessLevel]]:
        """Get next batch of files with their freshness levels."""
        batch = []
        now = datetime.now()
        
        if self.queue:
            oldest_time = self.queue[0][1]
            if now - oldest_time > self.max_age:
                logger.warning(f"BACKLOG: Oldest file is {(now - oldest_time).total_seconds() / 3600:.1f} hours old")
        
        for _ in range(min(batch_size, len(self.queue))):
            if self.queue:
                file_path, _, level, _ = self.queue.popleft()
                self._queued.discard(str(file_path))
                batch.append((file_path, level))
                self.processed_count += 1
        
        return batch
    
    def has_hot_or_urgent(self) -> bool:
        """Check if queue contains HOT or URGENT_WARM files."""
        return any(level in (FreshnessLevel.HOT, FreshnessLevel.URGENT_WARM) 
                  for _, _, level, _ in self.queue)
    
    def get_metrics(self) -> Dict[str, Any]:
        """Get queue metrics."""
        return {
            "queue_size": len(self.queue),
            "processed": self.processed_count,
            "deferred": self.deferred_count,
            "oldest_age_hours": self._get_oldest_age()
        }
    
    def _get_oldest_age(self) -> float:
        """Get age of oldest item in hours."""
        if not self.queue:
            return 0
        oldest_time = self.queue[0][1]
        return (datetime.now() - oldest_time).total_seconds() / 3600


class IndexingProgress:
    """Track progress toward 100% indexing."""
    
    def __init__(self, logs_dir: Path):
        self.logs_dir = logs_dir
        self.total_files = 0
        self.indexed_files = 0
        self.start_time = time.time()
        self.last_update = time.time()
        
    def scan_total_files(self) -> int:
        """Count total JSONL files."""
        total = 0
        if self.logs_dir.exists():
            for project_dir in self.logs_dir.iterdir():
                if project_dir.is_dir():
                    total += len(list(project_dir.glob("*.jsonl")))
        self.total_files = total
        return total
    
    def update(self, indexed_count: int):
        """Update progress."""
        self.indexed_files = indexed_count
        self.last_update = time.time()
    
    def get_progress(self) -> Dict[str, Any]:
        """Get progress metrics."""
        percent = (self.indexed_files / self.total_files * 100) if self.total_files > 0 else 0
        elapsed = time.time() - self.start_time
        rate = self.indexed_files / elapsed if elapsed > 0 else 0
        eta = (self.total_files - self.indexed_files) / rate if rate > 0 else 0
        
        return {
            'total_files': self.total_files,
            'indexed_files': self.indexed_files,
            'percent': percent,
            'rate_per_hour': rate * 3600,
            'eta_hours': eta / 3600,
            'elapsed_hours': elapsed / 3600
        }


class StreamingWatcher:
    """Production-ready streaming watcher with comprehensive monitoring."""
    
    def __init__(self, config: Config):
        self.config = config
        self.state: Dict[str, Any] = {}
        self.embedding_provider = self._create_embedding_provider()
        self.qdrant_service = QdrantService(config, self.embedding_provider)
        self.chunker = TokenAwareChunker()
        self.cpu_monitor = CPUMonitor(config.max_cpu_percent_per_core)
        self.memory_monitor = MemoryMonitor(config.memory_limit_mb, config.memory_warning_mb)
        self.queue_manager = QueueManager(config.max_queue_size, config.max_backlog_hours)
        self.progress = IndexingProgress(config.logs_dir)
        
        self.stats = {
            "files_processed": 0,
            "chunks_processed": 0,
            "failures": 0,
            "start_time": time.time()
        }
        
        # Track file wait times for starvation prevention
        self.file_first_seen: Dict[str, float] = {}
        self.current_project: Optional[str] = self._detect_current_project()
        self.last_mode: Optional[str] = None  # Track mode changes for logging
        
        self.shutdown_event = asyncio.Event()
        
        logger.info(f"Streaming Watcher v3.0.0 with HOT/WARM/COLD prioritization")
        logger.info(f"State file: {self.config.state_file}")
        logger.info(f"Memory limits: {config.memory_warning_mb}MB warning, {config.memory_limit_mb}MB limit")
        logger.info(f"HOT window: {config.hot_window_minutes} min, WARM window: {config.warm_window_hours} hrs")
    
    def _detect_current_project(self) -> Optional[str]:
        """Detect current project from working directory."""
        try:
            cwd = Path.cwd()
            # Check if we're in a claude project directory
            if "/.claude/projects/" in str(cwd):
                # Extract project name from path
                parts = str(cwd).split("/.claude/projects/")
                if len(parts) > 1:
                    project = parts[1].split("/")[0]
                    logger.info(f"Detected current project: {project}")
                    return project
        except Exception as e:
            logger.debug(f"Could not detect current project: {e}")
        return None
    
    def categorize_freshness(self, file_path: Path) -> Tuple[FreshnessLevel, int]:
        """
        Categorize file freshness for prioritization.
        Returns (FreshnessLevel, priority_score) where lower scores = higher priority.
        """
        now = time.time()
        file_key = str(file_path)
        
        # Track first seen time atomically
        if file_key not in self.file_first_seen:
            self.file_first_seen[file_key] = now
        first_seen_time = self.file_first_seen[file_key]
        
        file_age_minutes = (now - file_path.stat().st_mtime) / 60
        
        # Check if file is from current project
        is_current_project = False
        if self.current_project:
            file_project = normalize_project_name(str(file_path.parent))
            is_current_project = (file_project == self.current_project)
        
        # Determine base freshness level
        if file_age_minutes < self.config.hot_window_minutes:
            level = FreshnessLevel.HOT
            base_priority = 0  # Highest priority
        elif file_age_minutes < (self.config.warm_window_hours * 60):
            # Check for starvation (WARM files waiting too long)
            wait_minutes = (now - first_seen_time) / 60
            if wait_minutes > self.config.max_warm_wait_minutes:
                level = FreshnessLevel.URGENT_WARM
                base_priority = 1  # Second highest priority
            else:
                level = FreshnessLevel.WARM
                base_priority = 2 if is_current_project else 3
        else:
            level = FreshnessLevel.COLD
            base_priority = 4  # Lowest priority
        
        # Adjust priority score based on exact age for tie-breaking
        priority_score = base_priority * 10000 + min(file_age_minutes, 9999)
        
        return level, int(priority_score)
    
    def _create_embedding_provider(self) -> EmbeddingProvider:
        """Create embedding provider based on configuration."""
        if not self.config.prefer_local_embeddings and self.config.voyage_api_key:
            logger.info("Using Voyage AI for cloud embeddings")
            return VoyageProvider(
                api_key=self.config.voyage_api_key,
                model_name="voyage-3",  # Latest Voyage model with 1024 dimensions
                max_concurrent=self.config.max_concurrent_embeddings
            )
        else:
            logger.info(f"Using FastEmbed: {self.config.embedding_model}")
            return FastEmbedProvider(
                self.config.embedding_model,
                self.config.max_concurrent_embeddings
            )
    
    async def load_state(self) -> None:
        """Load persisted state with migration support."""
        if self.config.state_file.exists():
            try:
                with open(self.config.state_file, 'r') as f:
                    self.state = json.load(f)
                
                # Migrate old state format if needed
                if "imported_files" in self.state:
                    imported_count = len(self.state["imported_files"])
                    logger.info(f"Loaded state with {imported_count} files")
                    
                    # Ensure all entries have full paths as keys
                    migrated = {}
                    for key, value in self.state["imported_files"].items():
                        # Ensure key is a full path
                        if not key.startswith('/'):
                            # Try to reconstruct full path
                            possible_path = self.config.logs_dir / key
                            if possible_path.exists():
                                migrated[str(possible_path)] = value
                            else:
                                migrated[key] = value  # Keep as is
                        else:
                            migrated[key] = value
                    
                    if len(migrated) != len(self.state["imported_files"]):
                        logger.info(f"Migrated state format: {len(self.state['imported_files'])} -> {len(migrated)} entries")
                        self.state["imported_files"] = migrated
                        
            except Exception as e:
                logger.error(f"Error loading state: {e}")
                self.state = {}
        
        if "imported_files" not in self.state:
            self.state["imported_files"] = {}
        if "high_water_mark" not in self.state:
            self.state["high_water_mark"] = 0
        
        # Update progress tracker
        self.progress.update(len(self.state["imported_files"]))
    
    async def save_state(self) -> None:
        """Save state atomically."""
        try:
            self.config.state_file.parent.mkdir(parents=True, exist_ok=True)
            temp_file = self.config.state_file.with_suffix('.tmp')
            
            with open(temp_file, 'w') as f:
                json.dump(self.state, f, indent=2)
                f.flush()
                os.fsync(f.fileno())
            
            if platform.system() == 'Windows':
                if self.config.state_file.exists():
                    self.config.state_file.unlink()
                temp_file.rename(self.config.state_file)
            else:
                os.replace(temp_file, self.config.state_file)
            
            # Directory fsync for stronger guarantees
            try:
                dir_fd = os.open(str(self.config.state_file.parent), os.O_DIRECTORY)
                os.fsync(dir_fd)
                os.close(dir_fd)
            except:
                pass
                
        except Exception as e:
            logger.error(f"Error saving state: {e}")
    
    def get_collection_name(self, project_path: str) -> str:
        """Get collection name for project."""
        normalized = normalize_project_name(project_path)
        project_hash = hashlib.md5(normalized.encode()).hexdigest()[:8]
        suffix = "_local" if self.config.prefer_local_embeddings else "_voyage"
        return f"{self.config.collection_prefix}_{project_hash}{suffix}"
    
    def _extract_message_text(self, content: Any) -> str:
        """Extract text from message content."""
        if isinstance(content, str):
            return content
        elif isinstance(content, list):
            text_parts = []
            for item in content:
                if isinstance(item, str):
                    text_parts.append(item)
                elif isinstance(item, dict):
                    if item.get('type') == 'text':
                        text_parts.append(item.get('text', ''))
            return ' '.join(text_parts)
        return str(content) if content else ''
    
    async def process_file(self, file_path: Path) -> bool:
        """Process a single file."""
        try:
            # Memory check
            should_cleanup, mem_metrics = self.memory_monitor.check_memory()
            if should_cleanup:
                await self.memory_monitor.cleanup()
                _, mem_metrics = self.memory_monitor.check_memory()
                if mem_metrics['alert_level'] == 'critical':
                    logger.error(f"Memory critical: {mem_metrics['current_mb']:.1f}MB, skipping {file_path}")
                    return False
            
            project_path = str(file_path.parent)
            collection_name = self.get_collection_name(project_path)
            conversation_id = file_path.stem
            
            logger.info(f"Processing: {file_path.name} (memory: {mem_metrics['current_mb']:.1f}MB)")
            
            # Read messages (defer collection creation until we know we have content)
            all_messages = []
            with open(file_path, 'r') as f:
                for line in f:
                    if line.strip():
                        try:
                            data = json.loads(line)
                            # Handle 'messages' array (standard format)
                            if 'messages' in data and data['messages']:
                                all_messages.extend(data['messages'])
                            # Handle single 'message' object
                            elif 'message' in data and data['message']:
                                all_messages.append(data['message'])
                            # Handle direct role/content format
                            elif 'role' in data and 'content' in data:
                                all_messages.append(data)
                        except json.JSONDecodeError:
                            continue
            
            if not all_messages:
                logger.warning(f"No messages in {file_path}, marking as processed")
                # Mark file as processed with 0 chunks
                self.state["imported_files"][str(file_path)] = {
                    "imported_at": datetime.now().isoformat(),
                    "_parsed_time": datetime.now().timestamp(),
                    "chunks": 0,
                    "collection": collection_name,
                    "empty_file": True
                }
                self.stats["files_processed"] += 1
                return True
            
            # Extract metadata
            tool_usage = extract_tool_usage_from_conversation(all_messages)
            
            # Build text
            text_parts = []
            for msg in all_messages:
                role = msg.get('role', 'unknown')
                content = msg.get('content', '')
                text = self._extract_message_text(content)
                if text:
                    text_parts.append(f"{role}: {text}")
            
            combined_text = "\n\n".join(text_parts)
            if not combined_text.strip():
                logger.warning(f"No textual content in {file_path}, marking as processed")
                # Mark file as processed with 0 chunks (has messages but no extractable text)
                self.state["imported_files"][str(file_path)] = {
                    "imported_at": datetime.now().isoformat(),
                    "_parsed_time": datetime.now().timestamp(),
                    "chunks": 0,
                    "collection": collection_name,
                    "no_text_content": True
                }
                self.stats["files_processed"] += 1
                return True
            
            concepts = extract_concepts(combined_text, tool_usage)
            
            # Now we know we have content, ensure collection exists
            await self.qdrant_service.ensure_collection(collection_name)
            
            # Process chunks
            chunks_processed = 0
            chunk_index = 0
            
            for chunk_text in self.chunker.chunk_text_stream(combined_text):
                if self.shutdown_event.is_set():
                    return False
                
                # CPU throttling
                if self.cpu_monitor.should_throttle():
                    await asyncio.sleep(0.5)
                
                # Generate embedding
                embeddings = None
                for attempt in range(self.config.max_retries):
                    try:
                        embeddings = await self.embedding_provider.embed_documents([chunk_text])
                        # Validate embedding dimensions
                        if embeddings and len(embeddings[0]) != self.embedding_provider.vector_size:
                            logger.error(f"Embedding dimension mismatch: got {len(embeddings[0])}, expected {self.embedding_provider.vector_size} for provider {self.embedding_provider.__class__.__name__}")
                            self.stats["failures"] += 1
                            embeddings = None  # Force retry
                            continue  # Continue retrying, not break
                        break
                    except Exception as e:
                        logger.warning(f"Embed failed (attempt {attempt+1}): {e}")
                        if attempt < self.config.max_retries - 1:
                            await asyncio.sleep(self.config.retry_delay_s * (2 ** attempt))
                
                if not embeddings:
                    logger.error(f"Failed to embed chunk {chunk_index}")
                    self.stats["failures"] += 1
                    continue
                
                # Create payload
                payload = {
                    "text": chunk_text[:10000],
                    "conversation_id": conversation_id,
                    "chunk_index": chunk_index,
                    "message_count": len(all_messages),
                    "project": normalize_project_name(project_path),
                    "timestamp": datetime.now().isoformat(),
                    "total_length": len(chunk_text),
                    "chunking_version": "v3",
                    "concepts": concepts,
                    "files_analyzed": tool_usage['files_analyzed'],
                    "files_edited": tool_usage['files_edited'],
                    "tools_used": tool_usage['tools_used']
                }
                
                # Create point
                point_id_str = hashlib.md5(
                    f"{conversation_id}_{chunk_index}".encode()
                ).hexdigest()[:16]
                point_id = int(point_id_str, 16) % (2**63)
                
                point = models.PointStruct(
                    id=point_id,
                    vector=embeddings[0],
                    payload=payload
                )
                
                # Store
                success = await self.qdrant_service.store_points_with_retry(
                    collection_name,
                    [point]
                )
                
                if not success:
                    logger.error(f"Failed to store chunk {chunk_index}")
                    self.stats["failures"] += 1
                else:
                    chunks_processed += 1
                
                chunk_index += 1
                
                # Memory check mid-file
                if chunk_index % 10 == 0:
                    should_cleanup, _ = self.memory_monitor.check_memory()
                    if should_cleanup:
                        await self.memory_monitor.cleanup()
            
            # Update state - use full path as key
            self.state["imported_files"][str(file_path)] = {
                "imported_at": datetime.now().isoformat(),
                "_parsed_time": datetime.now().timestamp(),
                "chunks": chunks_processed,
                "collection": collection_name
            }
            
            self.stats["files_processed"] += 1
            self.stats["chunks_processed"] += chunks_processed
            
            logger.info(f"Completed: {file_path.name} ({chunks_processed} chunks)")
            return True
            
        except Exception as e:
            logger.error(f"Error processing {file_path}: {e}")
            self.stats["failures"] += 1
            return False
    
    async def find_new_files(self) -> List[Tuple[Path, FreshnessLevel, int]]:
        """Find new files to process with freshness categorization."""
        if not self.config.logs_dir.exists():
            logger.warning(f"Logs dir not found: {self.config.logs_dir}")
            return []
        
        categorized_files = []
        high_water_mark = self.state.get("high_water_mark", 0)
        new_high_water = high_water_mark
        now = time.time()
        
        try:
            for project_dir in self.config.logs_dir.iterdir():
                if not project_dir.is_dir():
                    continue
                
                try:
                    for jsonl_file in project_dir.glob("*.jsonl"):
                        file_mtime = jsonl_file.stat().st_mtime
                        new_high_water = max(new_high_water, file_mtime)
                        
                        # Check if already processed (using full path)
                        file_key = str(jsonl_file)
                        if file_key in self.state["imported_files"]:
                            stored = self.state["imported_files"][file_key]
                            if "_parsed_time" in stored:
                                if file_mtime <= stored["_parsed_time"]:
                                    continue
                            elif "imported_at" in stored:
                                import_time = datetime.fromisoformat(stored["imported_at"]).timestamp()
                                stored["_parsed_time"] = import_time
                                if file_mtime <= import_time:
                                    continue
                        
                        # Categorize file freshness (handles first_seen tracking internally)
                        freshness_level, priority_score = self.categorize_freshness(jsonl_file)
                        
                        categorized_files.append((jsonl_file, freshness_level, priority_score))
                except Exception as e:
                    logger.error(f"Error scanning project dir {project_dir}: {e}")
                    
        except Exception as e:
            logger.error(f"Error scanning logs dir: {e}")
        
        self.state["high_water_mark"] = new_high_water
        
        # Sort by priority score (lower = higher priority)
        categorized_files.sort(key=lambda x: x[2])
        
        # Log categorization summary
        if categorized_files:
            hot_count = sum(1 for _, level, _ in categorized_files if level == FreshnessLevel.HOT)
            urgent_count = sum(1 for _, level, _ in categorized_files if level == FreshnessLevel.URGENT_WARM)
            warm_count = sum(1 for _, level, _ in categorized_files if level == FreshnessLevel.WARM)
            cold_count = sum(1 for _, level, _ in categorized_files if level == FreshnessLevel.COLD)
            
            status_parts = []
            if hot_count: status_parts.append(f"{hot_count} 🔥HOT")
            if urgent_count: status_parts.append(f"{urgent_count} ⚠️URGENT")
            if warm_count: status_parts.append(f"{warm_count} 🌡️WARM")
            if cold_count: status_parts.append(f"{cold_count} ❄️COLD")
            
            logger.info(f"Found {len(categorized_files)} new files: {', '.join(status_parts)}")
        
        return categorized_files
    
    async def run_continuous(self) -> None:
        """Main loop with comprehensive monitoring."""
        logger.info("=" * 60)
        logger.info("Claude Self-Reflect Streaming Watcher v3.0.0")
        logger.info("=" * 60)
        logger.info(f"State file: {self.config.state_file}")
        logger.info(f"Memory: {self.config.memory_warning_mb}MB warning, {self.config.memory_limit_mb}MB limit")
        logger.info(f"CPU limit: {self.cpu_monitor.max_total_cpu:.1f}%")
        logger.info(f"Queue size: {self.config.max_queue_size}")
        logger.info("=" * 60)
        
        await self.load_state()
        
        # Initial progress scan
        total_files = self.progress.scan_total_files()
        indexed_files = len(self.state.get("imported_files", {}))
        self.progress.update(indexed_files)
        
        initial_progress = self.progress.get_progress()
        logger.info(f"Initial progress: {indexed_files}/{total_files} files ({initial_progress['percent']:.1f}%)")
        
        try:
            cycle_count = 0
            while not self.shutdown_event.is_set():
                try:
                    cycle_count += 1
                    
                    # Find new files with categorization
                    categorized_files = await self.find_new_files()
                    
                    # Determine if we have HOT files (in new files or existing queue)
                    has_hot_files = (any(level == FreshnessLevel.HOT for _, level, _ in categorized_files) 
                                   or self.queue_manager.has_hot_or_urgent())
                    
                    # Process files by temperature with proper priority
                    files_to_process = []
                    cold_count = 0
                    
                    for file_path, level, priority in categorized_files:
                        # Limit COLD files per cycle
                        if level == FreshnessLevel.COLD:
                            if cold_count >= self.config.max_cold_files:
                                logger.debug(f"Skipping COLD file {file_path.name} (limit reached)")
                                continue
                            cold_count += 1
                        
                        mod_time = datetime.fromtimestamp(file_path.stat().st_mtime)
                        files_to_process.append((file_path, mod_time, level, priority))
                    
                    if files_to_process:
                        added = self.queue_manager.add_categorized(files_to_process)
                        if added > 0:
                            logger.info(f"Cycle {cycle_count}: Added {added} files to queue")
                    
                    # Process batch
                    batch = self.queue_manager.get_batch(self.config.batch_size)
                    
                    for file_path, level in batch:
                        if self.shutdown_event.is_set():
                            break
                        
                        # Double-check if already imported (defensive)
                        file_key = str(file_path)
                        try:
                            file_mtime = file_path.stat().st_mtime
                        except FileNotFoundError:
                            logger.warning(f"File disappeared: {file_path}")
                            continue
                        
                        imported = self.state["imported_files"].get(file_key)
                        if imported:
                            parsed_time = imported.get("_parsed_time")
                            if not parsed_time and "imported_at" in imported:
                                parsed_time = datetime.fromisoformat(imported["imported_at"]).timestamp()
                            if parsed_time and file_mtime <= parsed_time:
                                logger.debug(f"Skipping already imported: {file_path.name}")
                                continue
                        
                        success = await self.process_file(file_path)
                        
                        if success:
                            # Clean up first_seen tracking to prevent memory leak
                            self.file_first_seen.pop(file_key, None)
                            await self.save_state()
                            self.progress.update(len(self.state["imported_files"]))
                    
                    # Log comprehensive metrics
                    if batch or cycle_count % 6 == 0:  # Every minute if idle
                        queue_metrics = self.queue_manager.get_metrics()
                        progress_metrics = self.progress.get_progress()
                        _, mem_metrics = self.memory_monitor.check_memory()
                        cpu = self.cpu_monitor.get_cpu_nowait()
                        
                        logger.info(
                            f"Progress: {progress_metrics['percent']:.1f}% "
                            f"({progress_metrics['indexed_files']}/{progress_metrics['total_files']}) | "
                            f"Queue: {queue_metrics['queue_size']} | "
                            f"Memory: {mem_metrics['current_mb']:.1f}MB/{mem_metrics['limit_mb']}MB | "
                            f"CPU: {cpu:.1f}% | "
                            f"Processed: {self.stats['files_processed']} | "
                            f"Failures: {self.stats['failures']}"
                        )
                        
                        # Alert on high memory
                        if mem_metrics['alert_level'] in ['warning', 'high', 'critical']:
                            logger.warning(
                                f"Memory {mem_metrics['alert_level'].upper()}: "
                                f"{mem_metrics['current_mb']:.1f}MB "
                                f"({mem_metrics['percent_of_limit']:.1f}% of limit)"
                            )
                        
                        # Progress toward 100%
                        if progress_metrics['percent'] >= 99.9:
                            logger.info("🎉 INDEXING COMPLETE: 100% of files processed!")
                        elif progress_metrics['percent'] >= 90:
                            logger.info(f"📈 Nearing completion: {progress_metrics['percent']:.1f}%")
                        
                        # Backlog alert
                        if queue_metrics['oldest_age_hours'] > self.config.max_backlog_hours:
                            logger.error(
                                f"BACKLOG CRITICAL: Oldest file is "
                                f"{queue_metrics['oldest_age_hours']:.1f} hours old"
                            )
                    
                    # Dynamic interval based on file temperature
                    current_mode = "HOT" if has_hot_files else "NORMAL"
                    
                    if current_mode != self.last_mode:
                        if has_hot_files:
                            logger.info(f"🔥 HOT files detected - switching to {self.config.hot_check_interval_s}s interval")
                        else:
                            logger.info(f"Returning to normal {self.config.import_frequency}s interval")
                        self.last_mode = current_mode
                    
                    wait_time = self.config.hot_check_interval_s if has_hot_files else self.config.import_frequency
                    
                    # Wait with interrupt capability for HOT files
                    try:
                        await asyncio.wait_for(
                            self.shutdown_event.wait(),
                            timeout=wait_time
                        )
                    except asyncio.TimeoutError:
                        pass  # Normal timeout, continue loop
                    
                except Exception as e:
                    logger.error(f"Error in main loop: {e}")
                    await asyncio.sleep(self.config.import_frequency)
                    
        except asyncio.CancelledError:
            logger.info("Main task cancelled")
            raise
        finally:
            logger.info("Shutting down...")
            await self.save_state()
            await self.embedding_provider.close()
            await self.qdrant_service.close()
            
            # Final metrics
            final_progress = self.progress.get_progress()
            logger.info("=" * 60)
            logger.info("Final Statistics:")
            logger.info(f"Progress: {final_progress['percent']:.1f}% complete")
            logger.info(f"Files processed: {self.stats['files_processed']}")
            logger.info(f"Chunks processed: {self.stats['chunks_processed']}")
            logger.info(f"Failures: {self.stats['failures']}")
            logger.info(f"Memory cleanups: {self.memory_monitor.cleanup_count}")
            logger.info(f"Peak memory: {self.memory_monitor.peak_memory:.1f}MB")
            logger.info("=" * 60)
            logger.info("Shutdown complete")
    
    async def shutdown(self):
        """Trigger graceful shutdown."""
        logger.info("Shutdown requested")
        self.shutdown_event.set()


async def main():
    """Main entry point."""
    config = Config()
    watcher = StreamingWatcher(config)
    
    # Setup signal handlers
    import signal
    
    loop = asyncio.get_running_loop()
    
    def shutdown_handler():
        logger.info("Received shutdown signal")
        watcher.shutdown_event.set()
    
    if hasattr(loop, "add_signal_handler"):
        for sig in (signal.SIGINT, signal.SIGTERM):
            loop.add_signal_handler(sig, shutdown_handler)
    else:
        # Windows fallback
        def signal_handler(sig, frame):
            logger.info(f"Received signal {sig}")
            watcher.shutdown_event.set()
        
        signal.signal(signal.SIGINT, signal_handler)
        signal.signal(signal.SIGTERM, signal_handler)
    
    try:
        await watcher.run_continuous()
    except (KeyboardInterrupt, asyncio.CancelledError):
        await watcher.shutdown()


if __name__ == "__main__":
    asyncio.run(main())