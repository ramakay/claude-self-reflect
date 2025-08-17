#!/usr/bin/env python3
"""
Production-Ready Streaming Importer v2.5.17 FINAL
Addresses all critical issues from Opus 4.1 and GPT-5 code reviews:
1. Fixed signal handler race condition
2. Fixed CPU monitoring initialization
3. Fixed queue overflow data loss
4. Fixed state persistence across restarts
5. Fixed cgroup-aware CPU detection
6. Fixed async operation cancellation
7. Fixed atomic file operations with fsync
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
    # FIXED: Use STATE_FILE env var with mounted /config default
    state_file: Path = field(default_factory=lambda: Path(os.getenv("STATE_FILE", "/config/streaming-state.json")))
    collection_prefix: str = "conv"
    vector_size: int = 384  # FastEmbed all-MiniLM-L6-v2
    
    # Production throttling controls
    import_frequency: int = field(default_factory=lambda: int(os.getenv("IMPORT_FREQUENCY", "10")))  # Check every 10s
    batch_size: int = field(default_factory=lambda: int(os.getenv("BATCH_SIZE", "5")))
    memory_limit_mb: int = field(default_factory=lambda: int(os.getenv("MEMORY_LIMIT_MB", "600")))
    
    # CPU management - properly scaled for multi-core
    max_cpu_percent_per_core: float = field(default_factory=lambda: float(os.getenv("MAX_CPU_PERCENT_PER_CORE", "50")))
    max_concurrent_embeddings: int = field(default_factory=lambda: int(os.getenv("MAX_CONCURRENT_EMBEDDINGS", "2")))
    max_concurrent_qdrant: int = field(default_factory=lambda: int(os.getenv("MAX_CONCURRENT_QDRANT", "3")))
    
    # Queue management
    max_queue_size: int = field(default_factory=lambda: int(os.getenv("MAX_QUEUE_SIZE", "100")))  # Max files in queue
    max_backlog_hours: int = field(default_factory=lambda: int(os.getenv("MAX_BACKLOG_HOURS", "24")))  # Alert if older
    
    # Reliability settings
    qdrant_timeout_s: float = field(default_factory=lambda: float(os.getenv("QDRANT_TIMEOUT", "10")))
    max_retries: int = field(default_factory=lambda: int(os.getenv("MAX_RETRIES", "3")))
    retry_delay_s: float = field(default_factory=lambda: float(os.getenv("RETRY_DELAY", "1")))
    
    # Collection cache settings
    collection_cache_ttl: int = field(default_factory=lambda: int(os.getenv("COLLECTION_CACHE_TTL", "3600")))  # 1 hour
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
    # Try to get from environment first
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
            # format: "<quota> <period>" or "max <period>"
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
        
        # Handle different content types
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
                        # Extract tool information
                        tool_name = item.get('name', '')
                        tool_usage['tools_used'].add(tool_name)
                        
                        # Extract file paths from tool inputs
                        if 'input' in item:
                            tool_input = item['input']
                            if isinstance(tool_input, dict):
                                # Check for file paths in common tool parameters
                                if 'file_path' in tool_input:
                                    file_path = tool_input['file_path']
                                    if tool_name in ['Read', 'Grep', 'Glob', 'LS']:
                                        tool_usage['files_analyzed'].append(file_path)
                                    elif tool_name in ['Edit', 'Write', 'MultiEdit']:
                                        tool_usage['files_edited'].append(file_path)
                                
                                # Handle multiple files
                                if 'files' in tool_input:
                                    files = tool_input['files']
                                    if isinstance(files, list):
                                        tool_usage['files_analyzed'].extend(files)
            text = ' '.join(text_parts)
        else:
            text = str(content)
        
        # Extract file paths from text content using regex
        file_patterns = [
            r'`([/\w\-\.]+\.\w+)`',
            r'File: ([/\w\-\.]+\.\w+)',
            r'(?:^|\s)(/[\w\-\./]+\.\w+)',
            r'(?:^|\s)([\w\-]+\.\w+)',
        ]
        
        for pattern in file_patterns:
            matches = re.findall(pattern, text[:5000])  # Limit regex to first 5k chars
            for match in matches[:10]:  # Limit matches
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
    
    # Limit text for concept extraction
    text_sample = text[:50000] if len(text) > 50000 else text
    
    concept_patterns = {
        'docker': r'\b(?:docker|container|compose|dockerfile)\b',
        'testing': r'\b(?:test|testing|unittest|pytest)\b',
        'database': r'\b(?:database|sql|postgres|mysql|mongodb)\b',
        'api': r'\b(?:api|rest|graphql|endpoint)\b',
        'security': r'\b(?:security|auth|authentication)\b',
        'performance': r'\b(?:performance|optimization|cache)\b',
        'debugging': r'\b(?:debug|debugging|error|bug)\b',
        'deployment': r'\b(?:deploy|deployment|ci\/cd)\b',
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
    
    async def embed_documents(self, texts: List[str]) -> List[List[float]]:
        """Generate embeddings with concurrency control and retry."""
        async with self.semaphore:
            loop = asyncio.get_event_loop()
            embeddings = await loop.run_in_executor(
                self.executor,
                lambda: list(self.model.embed(texts))
            )
            return [embedding.tolist() for embedding in embeddings]
    
    async def close(self):
        """Shutdown executor properly."""
        # FIXED: Use proper shutdown with wait=True
        self.executor.shutdown(wait=True, cancel_futures=True)


class QdrantService:
    """Qdrant service with proper backpressure and retries."""
    
    def __init__(self, config: Config, embedding_provider: EmbeddingProvider):
        self.config = config
        self.client = AsyncQdrantClient(url=config.qdrant_url)
        self.embedding_provider = embedding_provider
        self._collection_cache: Dict[str, float] = {}  # name -> timestamp
        self.request_semaphore = asyncio.Semaphore(config.max_concurrent_qdrant)
    
    async def ensure_collection(self, collection_name: str) -> None:
        """Ensure collection exists with TTL cache."""
        now = time.time()
        
        # Check cache with TTL
        if collection_name in self._collection_cache:
            if now - self._collection_cache[collection_name] < self.config.collection_cache_ttl:
                return
        
        # Enforce cache size limit
        if len(self._collection_cache) >= self.config.collection_cache_max_size:
            # Remove oldest entry
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
                # Create collection
                vector_size = 1024 if "_voyage" in collection_name else self.config.vector_size
                
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
        """Store points with retry logic and proper acknowledgment."""
        if not points:
            return True
        
        for attempt in range(self.config.max_retries):
            try:
                async with self.request_semaphore:
                    # FIXED: Create task for proper cancellation on timeout
                    task = asyncio.create_task(
                        self.client.upsert(
                            collection_name=collection_name,
                            points=points,
                            wait=True  # CRITICAL: Wait for acknowledgment
                        )
                    )
                    await asyncio.wait_for(task, timeout=self.config.qdrant_timeout_s)
                    logger.debug(f"Stored {len(points)} points in {collection_name}")
                    return True
                    
            except asyncio.TimeoutError:
                # FIXED: Cancel the background operation
                task.cancel()
                try:
                    await task
                except asyncio.CancelledError:
                    pass
                logger.warning(f"Timeout storing points (attempt {attempt + 1}/{self.config.max_retries})")
                if attempt < self.config.max_retries - 1:
                    await asyncio.sleep(self.config.retry_delay_s * (2 ** attempt))  # Exponential backoff
            except Exception as e:
                logger.error(f"Error storing points: {e}")
                if attempt < self.config.max_retries - 1:
                    await asyncio.sleep(self.config.retry_delay_s)
        
        return False
    
    async def close(self):
        """Close client connection."""
        # AsyncQdrantClient doesn't have explicit close, but we can clear cache
        self._collection_cache.clear()


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
                # Find natural boundary
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
        # FIXED: Use cgroup-aware CPU count
        effective_cores = get_effective_cpus()
        self.max_total_cpu = max_cpu_per_core * effective_cores
        logger.info(f"CPU Monitor: {effective_cores:.1f} effective cores, {self.max_total_cpu:.1f}% limit")
        
        # FIXED: Initialize CPU tracking properly
        self.process.cpu_percent(interval=None)  # First call to initialize
        time.sleep(0.01)  # Brief pause
        self.last_check = time.time()
        self.last_cpu = self.process.cpu_percent(interval=None)
    
    def get_cpu_nowait(self) -> float:
        """Get CPU without blocking (uses cached value)."""
        now = time.time()
        if now - self.last_check > 1.0:  # Update every second
            val = self.process.cpu_percent(interval=None)
            # FIXED: Guard against 0.0 from uninitialized reads
            if val == 0.0 and self.last_cpu == 0.0:
                # Best effort quick second sample
                time.sleep(0.01)
                val = self.process.cpu_percent(interval=None)
            self.last_cpu = val
            self.last_check = now
        return self.last_cpu
    
    def should_throttle(self) -> bool:
        """Check if we should throttle based on CPU."""
        return self.get_cpu_nowait() > self.max_total_cpu


class QueueManager:
    """Manage file processing queue with limits."""
    
    def __init__(self, max_size: int, max_age_hours: int):
        self.max_size = max_size
        self.max_age = timedelta(hours=max_age_hours)
        self.queue: deque = deque(maxlen=max_size)
        self.processed_count = 0
        self.deferred_count = 0  # FIXED: Track deferred vs dropped
    
    def add_files(self, files: List[Tuple[Path, datetime]]) -> int:
        """Add files to queue, return number added."""
        added = 0
        overflow = []
        
        for file_path, mod_time in files:
            if len(self.queue) >= self.max_size:
                overflow.append((file_path, mod_time))
            else:
                self.queue.append((file_path, mod_time))
                added += 1
        
        # FIXED: More accurate logging and alerting
        if overflow:
            self.deferred_count += len(overflow)
            oldest = min(overflow, key=lambda x: x[1])
            logger.critical(f"QUEUE OVERFLOW: {len(overflow)} files deferred to next cycle. "
                          f"Oldest: {oldest[0].name} ({(datetime.now() - oldest[1]).total_seconds() / 3600:.1f}h old). "
                          f"Consider increasing MAX_QUEUE_SIZE or BATCH_SIZE")
        
        return added
    
    def get_batch(self, batch_size: int) -> List[Path]:
        """Get next batch of files, prioritizing oldest."""
        batch = []
        now = datetime.now()
        
        # Check for stale files
        if self.queue:
            oldest_time = self.queue[0][1]
            if now - oldest_time > self.max_age:
                logger.warning(f"BACKLOG ALERT: Oldest file is {(now - oldest_time).total_seconds() / 3600:.1f} hours old")
        
        # Get batch (process oldest first)
        for _ in range(min(batch_size, len(self.queue))):
            if self.queue:
                file_path, _ = self.queue.popleft()
                batch.append(file_path)
                self.processed_count += 1
        
        return batch
    
    def get_metrics(self) -> Dict[str, Any]:
        """Get queue metrics."""
        return {
            "queue_size": len(self.queue),
            "processed": self.processed_count,
            "deferred": self.deferred_count,  # FIXED: Use deferred instead of dropped
            "oldest_age_hours": self._get_oldest_age()
        }
    
    def _get_oldest_age(self) -> float:
        """Get age of oldest item in hours."""
        if not self.queue:
            return 0
        oldest_time = self.queue[0][1]
        return (datetime.now() - oldest_time).total_seconds() / 3600


class StreamingImporter:
    """Production-ready streaming importer."""
    
    def __init__(self, config: Config):
        self.config = config
        self.state: Dict[str, Any] = {}
        self.embedding_provider = self._create_embedding_provider()
        self.qdrant_service = QdrantService(config, self.embedding_provider)
        self.chunker = TokenAwareChunker()
        self.cpu_monitor = CPUMonitor(config.max_cpu_percent_per_core)
        self.queue_manager = QueueManager(config.max_queue_size, config.max_backlog_hours)
        
        self.stats = {
            "files_processed": 0,
            "chunks_processed": 0,
            "failures": 0,
            "start_time": time.time()
        }
        
        self.shutdown_event = asyncio.Event()
    
    def _create_embedding_provider(self) -> EmbeddingProvider:
        """Create embedding provider with config."""
        if not self.config.prefer_local_embeddings and self.config.voyage_api_key:
            raise NotImplementedError("Voyage provider not yet implemented")
        else:
            logger.info(f"Using FastEmbed: {self.config.embedding_model}")
            return FastEmbedProvider(
                self.config.embedding_model,
                self.config.max_concurrent_embeddings
            )
    
    async def load_state(self) -> None:
        """Load persisted state."""
        if self.config.state_file.exists():
            try:
                with open(self.config.state_file, 'r') as f:
                    self.state = json.load(f)
                logger.info(f"Loaded state with {len(self.state.get('imported_files', {}))} files")
            except Exception as e:
                logger.error(f"Error loading state: {e}")
                self.state = {}
        
        if "imported_files" not in self.state:
            self.state["imported_files"] = {}
        if "high_water_mark" not in self.state:
            self.state["high_water_mark"] = 0
    
    async def save_state(self) -> None:
        """Save state atomically with fsync."""
        try:
            self.config.state_file.parent.mkdir(parents=True, exist_ok=True)
            temp_file = self.config.state_file.with_suffix('.tmp')
            
            # FIXED: Write with fsync for durability
            with open(temp_file, 'w') as f:
                json.dump(self.state, f, indent=2)
                f.flush()
                os.fsync(f.fileno())
            
            # FIXED: Platform-specific atomic replacement
            if platform.system() == 'Windows':
                # Windows requires explicit removal
                if self.config.state_file.exists():
                    self.config.state_file.unlink()
                temp_file.rename(self.config.state_file)
            else:
                # POSIX atomic rename
                os.replace(temp_file, self.config.state_file)
            
            # Optionally fsync directory for stronger guarantees
            try:
                dir_fd = os.open(str(self.config.state_file.parent), os.O_DIRECTORY)
                os.fsync(dir_fd)
                os.close(dir_fd)
            except:
                pass  # Directory fsync is best-effort
                
        except Exception as e:
            logger.error(f"Error saving state: {e}")
    
    def get_memory_usage_mb(self) -> float:
        """Get current memory usage."""
        process = psutil.Process()
        return process.memory_info().rss / 1024 / 1024
    
    async def memory_cleanup(self) -> None:
        """Perform memory cleanup."""
        collected = gc.collect()
        if MALLOC_TRIM_AVAILABLE:
            malloc_trim(0)
        logger.debug(f"Memory cleanup: collected {collected} objects")
    
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
        """Process a single file with proper error handling."""
        try:
            # FIXED: Memory check with GC overhead buffer
            memory_usage = self.get_memory_usage_mb()
            memory_threshold = self.config.memory_limit_mb * 0.85  # 15% buffer
            if memory_usage > memory_threshold:
                await self.memory_cleanup()
                if self.get_memory_usage_mb() > memory_threshold:
                    logger.warning(f"Memory limit exceeded ({memory_usage:.1f}MB > {memory_threshold:.1f}MB), skipping {file_path}")
                    return False
            
            project_path = str(file_path.parent)
            collection_name = self.get_collection_name(project_path)
            conversation_id = file_path.stem
            
            logger.info(f"Processing: {file_path.name}")
            
            await self.qdrant_service.ensure_collection(collection_name)
            
            # Read messages
            all_messages = []
            with open(file_path, 'r') as f:
                for line in f:
                    if line.strip():
                        try:
                            data = json.loads(line)
                            if 'message' in data and data['message']:
                                all_messages.append(data['message'])
                            elif 'role' in data and 'content' in data:
                                all_messages.append(data)
                        except json.JSONDecodeError:
                            continue
            
            if not all_messages:
                logger.warning(f"No messages in {file_path}")
                return True  # Mark as processed
            
            # Extract metadata
            tool_usage = extract_tool_usage_from_conversation(all_messages)
            
            # Build text efficiently
            text_parts = []
            for msg in all_messages:
                role = msg.get('role', 'unknown')
                content = msg.get('content', '')
                text = self._extract_message_text(content)
                if text:
                    text_parts.append(f"{role}: {text}")
            
            combined_text = "\n\n".join(text_parts)
            if not combined_text.strip():
                return True
            
            concepts = extract_concepts(combined_text, tool_usage)
            
            # Process chunks in streaming fashion
            chunks_processed = 0
            chunk_index = 0
            
            for chunk_text in self.chunker.chunk_text_stream(combined_text):
                # Check for shutdown
                if self.shutdown_event.is_set():
                    return False
                
                # CPU throttling
                if self.cpu_monitor.should_throttle():
                    await asyncio.sleep(0.5)
                
                # FIXED: Generate embedding with retry
                embeddings = None
                for attempt in range(self.config.max_retries):
                    try:
                        embeddings = await self.embedding_provider.embed_documents([chunk_text])
                        break
                    except Exception as e:
                        logger.warning(f"Embed failed (attempt {attempt+1}/{self.config.max_retries}): {e}")
                        if attempt < self.config.max_retries - 1:
                            await asyncio.sleep(self.config.retry_delay_s * (2 ** attempt))
                
                if not embeddings:
                    logger.error(f"Failed to embed chunk {chunk_index} for {conversation_id}")
                    self.stats["failures"] += 1
                    continue  # Skip this chunk but continue with others
                
                # Create payload
                payload = {
                    "text": chunk_text[:10000],  # Limit text size
                    "conversation_id": conversation_id,
                    "chunk_index": chunk_index,
                    "message_count": len(all_messages),
                    "project": normalize_project_name(project_path),
                    "timestamp": datetime.now().isoformat(),
                    "total_length": len(chunk_text),
                    "chunking_version": "v2",
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
                
                # Store with retry
                success = await self.qdrant_service.store_points_with_retry(
                    collection_name,
                    [point]
                )
                
                if not success:
                    logger.error(f"Failed to store chunk {chunk_index} for {conversation_id}")
                    self.stats["failures"] += 1
                else:
                    chunks_processed += 1
                
                chunk_index += 1
                
                # Memory check mid-file
                if chunk_index % 10 == 0:
                    if self.get_memory_usage_mb() > memory_threshold:
                        await self.memory_cleanup()
            
            # Update state with cached timestamp for efficiency
            self.state["imported_files"][str(file_path)] = {
                "imported_at": datetime.now().isoformat(),
                "_parsed_time": datetime.now().timestamp(),  # FIXED: Cache parsed timestamp
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
    
    async def find_new_files(self) -> List[Tuple[Path, datetime]]:
        """Find new files efficiently using high water mark."""
        # FIXED: Guard against missing logs_dir
        if not self.config.logs_dir.exists():
            logger.warning(f"Logs dir not found: {self.config.logs_dir}")
            return []
        
        new_files = []
        high_water_mark = self.state.get("high_water_mark", 0)
        new_high_water = high_water_mark
        
        try:
            for project_dir in self.config.logs_dir.iterdir():
                if not project_dir.is_dir():
                    continue
                
                try:
                    for jsonl_file in project_dir.glob("*.jsonl"):
                        file_mtime = jsonl_file.stat().st_mtime
                        new_high_water = max(new_high_water, file_mtime)
                        
                        # Skip if already processed (using cached timestamp)
                        if str(jsonl_file) in self.state["imported_files"]:
                            stored = self.state["imported_files"][str(jsonl_file)]
                            # FIXED: Use cached parsed timestamp for efficiency
                            if "_parsed_time" in stored:
                                if file_mtime <= stored["_parsed_time"]:
                                    continue
                            elif "imported_at" in stored:
                                import_time = datetime.fromisoformat(stored["imported_at"]).timestamp()
                                stored["_parsed_time"] = import_time  # Cache for next time
                                if file_mtime <= import_time:
                                    continue
                        
                        # Add to queue
                        new_files.append((jsonl_file, datetime.fromtimestamp(file_mtime)))
                except Exception as e:
                    logger.error(f"Error scanning project dir {project_dir}: {e}")
                    
        except Exception as e:
            logger.error(f"Error scanning logs dir {self.config.logs_dir}: {e}")
        
        # Update high water mark
        self.state["high_water_mark"] = new_high_water
        
        # Sort by age (oldest first) to prevent starvation
        new_files.sort(key=lambda x: x[1])
        
        return new_files
    
    async def run_continuous(self) -> None:
        """Main loop with proper shutdown handling."""
        logger.info("Starting production streaming importer v2.5.17 FINAL")
        logger.info(f"CPU limit: {self.cpu_monitor.max_total_cpu:.1f}%")
        logger.info(f"Queue size: {self.config.max_queue_size}")
        logger.info(f"State file: {self.config.state_file}")
        
        await self.load_state()
        
        try:
            while not self.shutdown_event.is_set():
                try:
                    # Find new files
                    new_files = await self.find_new_files()
                    
                    if new_files:
                        added = self.queue_manager.add_files(new_files)
                        logger.info(f"Added {added} files to queue")
                    
                    # Process batch
                    batch = self.queue_manager.get_batch(self.config.batch_size)
                    
                    for file_path in batch:
                        if self.shutdown_event.is_set():
                            break
                        
                        success = await self.process_file(file_path)
                        
                        # Save state after each file for durability
                        if success:
                            await self.save_state()
                    
                    # Log metrics
                    if batch:
                        metrics = self.queue_manager.get_metrics()
                        cpu = self.cpu_monitor.get_cpu_nowait()
                        mem = self.get_memory_usage_mb()
                        logger.info(f"Metrics: Queue={metrics['queue_size']}, "
                                   f"CPU={cpu:.1f}%, Mem={mem:.1f}MB, "
                                   f"Processed={self.stats['files_processed']}, "
                                   f"Failures={self.stats['failures']}")
                        
                        if metrics['oldest_age_hours'] > self.config.max_backlog_hours:
                            logger.error(f"BACKLOG ALERT: Oldest file is {metrics['oldest_age_hours']:.1f} hours old")
                    
                    # Wait before next cycle
                    await asyncio.sleep(self.config.import_frequency)
                    
                except Exception as e:
                    logger.error(f"Error in main loop: {e}")
                    await asyncio.sleep(self.config.import_frequency)
                    
        except asyncio.CancelledError:
            # FIXED: Handle CancelledError properly
            logger.info("Main task cancelled")
            raise
        finally:
            # Cleanup
            logger.info("Shutting down...")
            await self.save_state()
            await self.embedding_provider.close()
            await self.qdrant_service.close()
            logger.info("Shutdown complete")
    
    async def shutdown(self):
        """Trigger graceful shutdown."""
        logger.info("Shutdown requested")
        self.shutdown_event.set()


async def main():
    """Main entry point with signal handling."""
    config = Config()
    importer = StreamingImporter(config)
    
    # FIXED: Setup signal handlers using asyncio-native approach
    import signal
    
    loop = asyncio.get_running_loop()
    
    # Define shutdown handler
    def shutdown_handler():
        logger.info("Received shutdown signal")
        importer.shutdown_event.set()
    
    # Use asyncio-native signal handling on Unix
    if hasattr(loop, "add_signal_handler"):
        for sig in (signal.SIGINT, signal.SIGTERM):
            loop.add_signal_handler(sig, shutdown_handler)
    else:
        # Fallback for Windows
        def signal_handler(sig, frame):
            logger.info(f"Received signal {sig}")
            # Set the shutdown event directly - it's thread-safe
            importer.shutdown_event.set()
        
        signal.signal(signal.SIGINT, signal_handler)
        signal.signal(signal.SIGTERM, signal_handler)
    
    try:
        await importer.run_continuous()
    except (KeyboardInterrupt, asyncio.CancelledError):
        await importer.shutdown()


if __name__ == "__main__":
    asyncio.run(main())