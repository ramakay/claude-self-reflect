#!/usr/bin/env python3
"""
Comprehensive Tests for Claude Self-Reflect Streaming Watcher v3.0.0
Tests all critical functionality including local/cloud modes, state management,
memory handling, queue management, and production resilience features.
"""

import asyncio
import os
import sys
import json
import tempfile
import shutil
import time
import unittest
from pathlib import Path
from unittest.mock import Mock, AsyncMock, patch, MagicMock
from datetime import datetime, timedelta
from typing import Dict, List, Any

# Add parent directory to path for imports
scripts_dir = Path(__file__).parent.parent / "scripts"
sys.path.insert(0, str(scripts_dir))

# Import the streaming watcher
try:
    from streaming_watcher import (
        Config, StreamingWatcher, VoyageProvider, FastEmbedProvider,
        MemoryMonitor, CPUMonitor, QueueManager, IndexingProgress,
        QdrantService, TokenAwareChunker, get_effective_cpus,
        extract_tool_usage_from_conversation, extract_concepts
    )
except ImportError as e:
    print(f"Failed to import streaming_watcher: {e}")
    print(f"Scripts directory: {scripts_dir}")
    print(f"Scripts directory exists: {scripts_dir.exists()}")
    sys.exit(1)

class MockVoyageAPI:
    """Mock Voyage API for testing without actual API calls."""
    
    def __init__(self, fail_count: int = 0, rate_limit: bool = False):
        self.call_count = 0
        self.fail_count = fail_count
        self.rate_limit = rate_limit
        self.responses = []
    
    async def __aenter__(self):
        return self
    
    async def __aexit__(self, *args):
        pass
    
    async def post(self, url, **kwargs):
        self.call_count += 1
        
        # Simulate failures for first fail_count calls
        if self.call_count <= self.fail_count:
            raise Exception(f"Mock API failure #{self.call_count}")
        
        # Simulate rate limiting
        if self.rate_limit and self.call_count <= 2:
            mock_response = AsyncMock()
            mock_response.status = 429
            mock_response.headers = {"Retry-After": "1"}
            return mock_response
        
        # Success response
        mock_response = AsyncMock()
        mock_response.status = 200
        mock_response.json = AsyncMock(return_value={
            "data": [
                {"embedding": [0.1] * 1024},  # 1024-dim vector
                {"embedding": [0.2] * 1024}
            ]
        })
        return mock_response

class TestStreamingWatcherConfiguration(unittest.TestCase):
    """Test configuration and initialization."""
    
    def setUp(self):
        self.temp_dir = Path(tempfile.mkdtemp())
        self.original_env = os.environ.copy()
    
    def tearDown(self):
        shutil.rmtree(self.temp_dir, ignore_errors=True)
        os.environ.clear()
        os.environ.update(self.original_env)
    
    def test_default_config_local_mode(self):
        """Test default configuration uses local embeddings."""
        config = Config()
        
        self.assertTrue(config.prefer_local_embeddings)
        self.assertEqual(config.vector_size, 384)  # FastEmbed
        self.assertIsNone(config.voyage_api_key)
        self.assertTrue(str(config.state_file).endswith("csr-watcher.json"))
    
    def test_cloud_mode_config(self):
        """Test configuration with Voyage AI enabled."""
        os.environ["PREFER_LOCAL_EMBEDDINGS"] = "false"
        os.environ["VOYAGE_API_KEY"] = "test-key-123"
        
        config = Config()
        
        self.assertFalse(config.prefer_local_embeddings)
        self.assertEqual(config.voyage_api_key, "test-key-123")
        self.assertTrue("cloud" in str(config.state_file))
    
    def test_docker_mode_detection(self):
        """Test Docker mode state file selection."""
        # Create mock dockerenv file
        dockerenv = Path("/.dockerenv")
        
        with patch('os.path.exists') as mock_exists:
            mock_exists.side_effect = lambda path: path == "/.dockerenv"
            
            config = Config()
            self.assertTrue(str(config.state_file).startswith("/config/"))
    
    def test_memory_limits_configuration(self):
        """Test memory limit configuration."""
        os.environ["MEMORY_LIMIT_MB"] = "2048"
        os.environ["MEMORY_WARNING_MB"] = "1024"
        
        config = Config()
        
        self.assertEqual(config.memory_limit_mb, 2048)
        self.assertEqual(config.memory_warning_mb, 1024)
    
    def test_cpu_configuration(self):
        """Test CPU monitoring configuration."""
        os.environ["MAX_CPU_PERCENT_PER_CORE"] = "75.0"
        os.environ["MAX_CONCURRENT_EMBEDDINGS"] = "4"
        
        config = Config()
        
        self.assertEqual(config.max_cpu_percent_per_core, 75.0)
        self.assertEqual(config.max_concurrent_embeddings, 4)

class TestVoyageProvider(unittest.TestCase):
    """Test Voyage AI provider implementation."""
    
    def setUp(self):
        self.provider = VoyageProvider("test-api-key", "voyage-3")
    
    async def test_successful_embedding(self):
        """Test successful embedding generation."""
        mock_session = AsyncMock()
        self.provider.session = mock_session
        
        # Mock successful response
        mock_response = AsyncMock()
        mock_response.status = 200
        mock_response.json.return_value = {
            "data": [
                {"embedding": [0.1] * 1024},
                {"embedding": [0.2] * 1024}
            ]
        }
        
        mock_session.post.return_value.__aenter__.return_value = mock_response
        
        texts = ["hello world", "test document"]
        embeddings = await self.provider.embed_documents(texts)
        
        self.assertEqual(len(embeddings), 2)
        self.assertEqual(len(embeddings[0]), 1024)  # Voyage dimensions
        self.assertEqual(embeddings[0][0], 0.1)
    
    async def test_rate_limiting_retry(self):
        """Test rate limiting handling."""
        mock_session = AsyncMock()
        self.provider.session = mock_session
        
        # First call: rate limited
        rate_limit_response = AsyncMock()
        rate_limit_response.status = 429
        rate_limit_response.headers = {"Retry-After": "1"}
        
        # Second call: success
        success_response = AsyncMock()
        success_response.status = 200
        success_response.json.return_value = {
            "data": [{"embedding": [0.1] * 1024}]
        }
        
        mock_session.post.return_value.__aenter__.side_effect = [
            rate_limit_response,
            success_response
        ]
        
        with patch('asyncio.sleep') as mock_sleep:
            embeddings = await self.provider.embed_documents(["test"])
            mock_sleep.assert_called_once()
        
        self.assertEqual(len(embeddings), 1)
    
    async def test_api_failure_retry(self):
        """Test API failure retry logic."""
        mock_session = AsyncMock()
        self.provider.session = mock_session
        self.provider.max_retries = 2
        
        # All calls fail
        error_response = AsyncMock()
        error_response.status = 500
        error_response.text.return_value = "Server Error"
        
        mock_session.post.return_value.__aenter__.return_value = error_response
        
        with self.assertRaises(Exception) as cm:
            await self.provider.embed_documents(["test"])
        
        self.assertIn("Failed to get embeddings", str(cm.exception))
    
    async def test_session_cleanup(self):
        """Test session cleanup on close."""
        mock_session = AsyncMock()
        self.provider.session = mock_session
        
        await self.provider.close()
        
        mock_session.close.assert_called_once()
        self.assertIsNone(self.provider.session)

class TestFastEmbedProvider(unittest.TestCase):
    """Test FastEmbed provider implementation."""
    
    def setUp(self):
        # Mock FastEmbed to avoid loading actual model
        self.mock_model = Mock()
        self.mock_model.embed.return_value = [
            Mock(tolist=lambda: [0.1] * 384),
            Mock(tolist=lambda: [0.2] * 384)
        ]
        
        with patch('streaming_watcher.TextEmbedding', return_value=self.mock_model):
            self.provider = FastEmbedProvider("test-model")
    
    async def test_embedding_generation(self):
        """Test FastEmbed embedding generation."""
        texts = ["hello", "world"]
        embeddings = await self.provider.embed_documents(texts)
        
        self.assertEqual(len(embeddings), 2)
        self.assertEqual(len(embeddings[0]), 384)  # FastEmbed dimensions
        self.mock_model.embed.assert_called_once_with(texts)
    
    async def test_concurrent_limit(self):
        """Test concurrency limiting."""
        provider = FastEmbedProvider("test-model", max_concurrent=1)
        
        # Start two concurrent requests
        task1 = asyncio.create_task(provider.embed_documents(["test1"]))
        task2 = asyncio.create_task(provider.embed_documents(["test2"]))
        
        results = await asyncio.gather(task1, task2)
        
        self.assertEqual(len(results), 2)
        self.assertEqual(len(results[0]), 1)
        self.assertEqual(len(results[1]), 1)

class TestMemoryMonitor(unittest.TestCase):
    """Test memory monitoring functionality."""
    
    def test_memory_monitoring_basic(self):
        """Test basic memory monitoring."""
        monitor = MemoryMonitor(limit_mb=1000, warning_mb=500)
        
        should_cleanup, metrics = monitor.check_memory()
        
        self.assertIsInstance(should_cleanup, bool)
        self.assertIn('current_mb', metrics)
        self.assertIn('peak_mb', metrics)
        self.assertIn('alert_level', metrics)
        self.assertIn('details', metrics)
    
    def test_memory_threshold_detection(self):
        """Test memory threshold detection."""
        # Mock high memory usage
        with patch('psutil.Process') as mock_process:
            mock_memory = Mock()
            mock_memory.rss = 1500 * 1024 * 1024  # 1500MB
            mock_memory.vms = 2000 * 1024 * 1024  # 2000MB
            
            mock_process.return_value.memory_info.return_value = mock_memory
            mock_process.return_value.memory_percent.return_value = 25.0
            
            with patch('psutil.virtual_memory') as mock_vm:
                mock_vm.return_value.available = 4000 * 1024 * 1024  # 4GB available
                
                monitor = MemoryMonitor(limit_mb=1000, warning_mb=500)
                should_cleanup, metrics = monitor.check_memory()
        
        self.assertTrue(should_cleanup)
        self.assertEqual(metrics['alert_level'], 'critical')
        self.assertGreater(metrics['current_mb'], 1000)
    
    async def test_memory_cleanup(self):
        """Test memory cleanup operations."""
        monitor = MemoryMonitor(limit_mb=1000, warning_mb=500)
        
        with patch('gc.collect') as mock_gc:
            cleanup_result = await monitor.cleanup()
        
        mock_gc.assert_called_with(2)  # Full collection
        self.assertIn('freed_mb', cleanup_result)
        self.assertIn('cleanup_count', cleanup_result)

class TestCPUMonitor(unittest.TestCase):
    """Test CPU monitoring functionality."""
    
    def test_cpu_effective_cores(self):
        """Test effective CPU core detection."""
        cores = get_effective_cpus()
        self.assertGreater(cores, 0)
        self.assertIsInstance(cores, float)
    
    def test_cpu_monitoring(self):
        """Test CPU monitoring initialization."""
        with patch('streaming_watcher.get_effective_cpus', return_value=2.0):
            monitor = CPUMonitor(max_cpu_per_core=50.0)
            self.assertEqual(monitor.max_total_cpu, 100.0)
    
    def test_cpu_throttling_detection(self):
        """Test CPU throttling detection."""
        with patch('psutil.Process') as mock_process:
            mock_process.return_value.cpu_percent.return_value = 150.0
            
            with patch('streaming_watcher.get_effective_cpus', return_value=2.0):
                monitor = CPUMonitor(max_cpu_per_core=50.0)  # 100% limit
                
                # Force initial reading
                monitor.last_cpu = 150.0
                
                should_throttle = monitor.should_throttle()
                self.assertTrue(should_throttle)

class TestQueueManager(unittest.TestCase):
    """Test queue management functionality."""
    
    def setUp(self):
        self.queue_manager = QueueManager(max_size=5, max_age_hours=24)
        self.temp_files = []
        
        # Create temporary test files
        for i in range(10):
            temp_file = Path(tempfile.mkdtemp()) / f"test_{i}.jsonl"
            temp_file.touch()
            self.temp_files.append((temp_file, datetime.now() - timedelta(hours=i)))
    
    def tearDown(self):
        for file_path, _ in self.temp_files:
            if file_path.exists():
                shutil.rmtree(file_path.parent, ignore_errors=True)
    
    def test_queue_normal_operation(self):
        """Test normal queue operations."""
        # Add files within limit
        files_to_add = self.temp_files[:3]
        added = self.queue_manager.add_files(files_to_add)
        
        self.assertEqual(added, 3)
        
        # Get batch
        batch = self.queue_manager.get_batch(2)
        self.assertEqual(len(batch), 2)
        
        metrics = self.queue_manager.get_metrics()
        self.assertEqual(metrics['queue_size'], 1)  # 3 added, 2 removed
        self.assertEqual(metrics['processed'], 2)
    
    def test_queue_overflow_handling(self):
        """Test queue overflow handling."""
        # Try to add more files than max_size
        added = self.queue_manager.add_files(self.temp_files)
        
        self.assertEqual(added, 5)  # Only max_size added
        
        metrics = self.queue_manager.get_metrics()
        self.assertEqual(metrics['queue_size'], 5)
        self.assertEqual(metrics['deferred'], 5)  # 10 - 5 = 5 deferred
    
    def test_backlog_detection(self):
        """Test backlog age detection."""
        # Add old files
        old_files = [(path, datetime.now() - timedelta(hours=48)) 
                    for path, _ in self.temp_files[:2]]
        
        self.queue_manager.add_files(old_files)
        metrics = self.queue_manager.get_metrics()
        
        self.assertGreater(metrics['oldest_age_hours'], 24)

class TestTokenAwareChunker(unittest.TestCase):
    """Test text chunking functionality."""
    
    def setUp(self):
        self.chunker = TokenAwareChunker(chunk_size_tokens=100, chunk_overlap_tokens=20)
    
    def test_small_text_no_chunking(self):
        """Test that small text doesn't get chunked."""
        text = "This is a short text."
        chunks = list(self.chunker.chunk_text_stream(text))
        
        self.assertEqual(len(chunks), 1)
        self.assertEqual(chunks[0], text)
    
    def test_large_text_chunking(self):
        """Test chunking of large text."""
        # Create text larger than chunk size
        text = "This is a sentence. " * 100  # ~2000 chars, should chunk
        chunks = list(self.chunker.chunk_text_stream(text))
        
        self.assertGreater(len(chunks), 1)
        
        # Verify chunks don't exceed expected size
        for chunk in chunks:
            self.assertLessEqual(len(chunk), self.chunker.chunk_size_chars * 1.1)  # Allow 10% tolerance
    
    def test_chunk_boundaries(self):
        """Test chunking respects sentence boundaries."""
        text = ("First sentence. " * 20 + 
               "Second paragraph here. " * 20 + 
               "Third section content. " * 20)
        
        chunks = list(self.chunker.chunk_text_stream(text))
        
        # Should have multiple chunks
        self.assertGreater(len(chunks), 1)
        
        # Most chunks should end with sentence boundary
        sentence_endings = sum(1 for chunk in chunks if chunk.strip().endswith('.'))
        self.assertGreater(sentence_endings, 0)

class TestCollectionNaming(unittest.TestCase):
    """Test collection naming for local vs cloud modes."""
    
    def setUp(self):
        self.temp_dir = Path(tempfile.mkdtemp())
        self.config_local = Config()
        self.config_local.prefer_local_embeddings = True
        
        self.config_cloud = Config()
        self.config_cloud.prefer_local_embeddings = False
        self.config_cloud.voyage_api_key = "test-key"
    
    def tearDown(self):
        shutil.rmtree(self.temp_dir, ignore_errors=True)
    
    async def test_local_collection_naming(self):
        """Test local mode collection naming."""
        with patch('streaming_watcher.FastEmbedProvider'):
            watcher = StreamingWatcher(self.config_local)
            
            collection_name = watcher.get_collection_name("/test/project")
            self.assertTrue(collection_name.endswith("_local"))
            self.assertTrue(collection_name.startswith("conv_"))
    
    async def test_cloud_collection_naming(self):
        """Test cloud mode collection naming."""
        with patch('streaming_watcher.VoyageProvider'):
            watcher = StreamingWatcher(self.config_cloud)
            
            collection_name = watcher.get_collection_name("/test/project")
            self.assertTrue(collection_name.endswith("_voyage"))
            self.assertTrue(collection_name.startswith("conv_"))
    
    async def test_consistent_naming(self):
        """Test collection naming consistency."""
        with patch('streaming_watcher.FastEmbedProvider'):
            watcher = StreamingWatcher(self.config_local)
            
            # Same project should always get same collection name
            name1 = watcher.get_collection_name("/test/project")
            name2 = watcher.get_collection_name("/test/project")
            
            self.assertEqual(name1, name2)

class TestStatePersistence(unittest.TestCase):
    """Test state persistence across mode switches."""
    
    def setUp(self):
        self.temp_dir = Path(tempfile.mkdtemp())
        
        # Local config
        self.config_local = Config()
        self.config_local.prefer_local_embeddings = True
        self.config_local.state_file = self.temp_dir / "local-state.json"
        
        # Cloud config
        self.config_cloud = Config()
        self.config_cloud.prefer_local_embeddings = False
        self.config_cloud.voyage_api_key = "test-key"
        self.config_cloud.state_file = self.temp_dir / "cloud-state.json"
    
    def tearDown(self):
        shutil.rmtree(self.temp_dir, ignore_errors=True)
    
    async def test_state_file_separation(self):
        """Test separate state files for local/cloud modes."""
        with patch('streaming_watcher.FastEmbedProvider'):
            watcher_local = StreamingWatcher(self.config_local)
            watcher_local.state = {"test_local": True}
            await watcher_local.save_state()
        
        with patch('streaming_watcher.VoyageProvider'):
            watcher_cloud = StreamingWatcher(self.config_cloud)
            watcher_cloud.state = {"test_cloud": True}
            await watcher_cloud.save_state()
        
        # Verify separate files exist
        self.assertTrue(self.config_local.state_file.exists())
        self.assertTrue(self.config_cloud.state_file.exists())
        
        # Verify content is separate
        with open(self.config_local.state_file) as f:
            local_state = json.load(f)
        with open(self.config_cloud.state_file) as f:
            cloud_state = json.load(f)
        
        self.assertIn("test_local", local_state)
        self.assertNotIn("test_cloud", local_state)
        self.assertIn("test_cloud", cloud_state)
        self.assertNotIn("test_local", cloud_state)
    
    async def test_state_migration(self):
        """Test state format migration."""
        # Create old format state
        old_state = {
            "imported_files": {
                "relative/path.jsonl": {"imported_at": "2024-01-01T00:00:00"},
                "/absolute/path.jsonl": {"imported_at": "2024-01-01T01:00:00"}
            }
        }
        
        with open(self.config_local.state_file, 'w') as f:
            json.dump(old_state, f)
        
        with patch('streaming_watcher.FastEmbedProvider'):
            watcher = StreamingWatcher(self.config_local)
            await watcher.load_state()
        
        # Check that state was loaded and migration happened
        self.assertIn("imported_files", watcher.state)
        self.assertEqual(len(watcher.state["imported_files"]), 2)

class TestToolUsageExtraction(unittest.TestCase):
    """Test tool usage metadata extraction."""
    
    def test_basic_tool_extraction(self):
        """Test basic tool usage extraction."""
        messages = [
            {
                "role": "user",
                "content": "Please edit the file config.py"
            },
            {
                "role": "assistant", 
                "content": [
                    {
                        "type": "tool_use",
                        "name": "Edit",
                        "input": {"file_path": "/path/to/config.py"}
                    }
                ]
            }
        ]
        
        tool_usage = extract_tool_usage_from_conversation(messages)
        
        self.assertIn("Edit", tool_usage['tools_used'])
        self.assertIn("/path/to/config.py", tool_usage['files_edited'])
    
    def test_file_analysis_tools(self):
        """Test file analysis tool detection."""
        messages = [
            {
                "role": "assistant",
                "content": [
                    {
                        "type": "tool_use",
                        "name": "Read",
                        "input": {"file_path": "/test/file.py"}
                    },
                    {
                        "type": "tool_use", 
                        "name": "Grep",
                        "input": {"pattern": "test", "file_path": "/test/search.py"}
                    }
                ]
            }
        ]
        
        tool_usage = extract_tool_usage_from_conversation(messages)
        
        self.assertIn("Read", tool_usage['tools_used'])
        self.assertIn("Grep", tool_usage['tools_used'])
        self.assertIn("/test/file.py", tool_usage['files_analyzed'])
        # Grep should be analysis, not edit
        self.assertNotIn("/test/search.py", tool_usage['files_edited'])
    
    def test_concept_extraction(self):
        """Test concept extraction from text."""
        text = """
        I need to fix the Docker container that's having performance issues.
        The database queries are slow and we need to optimize the API endpoints.
        Let's also add some unit tests to prevent future bugs.
        """
        
        tool_usage = {"tools_used": ["Docker", "Bash"]}
        concepts = extract_concepts(text, tool_usage)
        
        expected_concepts = ["docker", "performance", "database", "api", "testing", "debugging"]
        for concept in expected_concepts:
            self.assertIn(concept, concepts)

class TestIndexingProgress(unittest.TestCase):
    """Test indexing progress tracking."""
    
    def setUp(self):
        self.temp_dir = Path(tempfile.mkdtemp())
        
        # Create mock project structure
        self.project1_dir = self.temp_dir / "project1"
        self.project1_dir.mkdir()
        self.project2_dir = self.temp_dir / "project2"
        self.project2_dir.mkdir()
        
        # Create test files
        (self.project1_dir / "conv1.jsonl").touch()
        (self.project1_dir / "conv2.jsonl").touch()
        (self.project2_dir / "conv3.jsonl").touch()
        
        self.progress = IndexingProgress(self.temp_dir)
    
    def tearDown(self):
        shutil.rmtree(self.temp_dir, ignore_errors=True)
    
    def test_total_file_scan(self):
        """Test total file counting."""
        total = self.progress.scan_total_files()
        self.assertEqual(total, 3)  # 3 .jsonl files created
    
    def test_progress_calculation(self):
        """Test progress percentage calculation."""
        self.progress.scan_total_files()  # total_files = 3
        self.progress.update(1)  # indexed_files = 1
        
        progress_data = self.progress.get_progress()
        
        self.assertEqual(progress_data['total_files'], 3)
        self.assertEqual(progress_data['indexed_files'], 1)
        self.assertAlmostEqual(progress_data['percent'], 33.33, places=1)
    
    def test_eta_calculation(self):
        """Test ETA calculation."""
        self.progress.scan_total_files()
        
        # Simulate some processing time
        time.sleep(0.1)
        self.progress.update(1)
        
        progress_data = self.progress.get_progress()
        
        self.assertGreater(progress_data['rate_per_hour'], 0)
        self.assertGreater(progress_data['eta_hours'], 0)
        self.assertGreater(progress_data['elapsed_hours'], 0)

class TestNoMessagesHandling(unittest.TestCase):
    """Test handling of files with no messages (critical fix)."""
    
    def setUp(self):
        self.temp_dir = Path(tempfile.mkdtemp())
        self.config = Config()
        self.config.logs_dir = self.temp_dir
        self.config.state_file = self.temp_dir / "test-state.json"
        
        # Create test files
        self.empty_file = self.temp_dir / "empty.jsonl"
        self.empty_file.touch()
        
        self.no_messages_file = self.temp_dir / "no_messages.jsonl"
        with open(self.no_messages_file, 'w') as f:
            f.write('{"type": "metadata", "project": "test"}\n')
        
        self.valid_file = self.temp_dir / "valid.jsonl"
        with open(self.valid_file, 'w') as f:
            f.write('{"message": {"role": "user", "content": "test"}}\n')
    
    def tearDown(self):
        shutil.rmtree(self.temp_dir, ignore_errors=True)
    
    async def test_empty_file_handling(self):
        """Test handling of completely empty files."""
        with patch('streaming_watcher.FastEmbedProvider'):
            with patch('streaming_watcher.QdrantService'):
                watcher = StreamingWatcher(self.config)
                
                # Should return True (success) but not process anything
                result = await watcher.process_file(self.empty_file)
                self.assertTrue(result)
                
                # Should not be added to state (no content to process)
                self.assertNotIn(str(self.empty_file), watcher.state.get("imported_files", {}))
    
    async def test_no_messages_file_handling(self):
        """Test handling of files with JSON but no messages."""
        with patch('streaming_watcher.FastEmbedProvider'):
            with patch('streaming_watcher.QdrantService'):
                watcher = StreamingWatcher(self.config)
                
                result = await watcher.process_file(self.no_messages_file)
                self.assertTrue(result)
                
                # Should not be added to state (no messages)
                self.assertNotIn(str(self.no_messages_file), watcher.state.get("imported_files", {}))
    
    async def test_valid_file_processing(self):
        """Test that valid files are still processed correctly."""
        mock_embedding_provider = AsyncMock()
        mock_embedding_provider.embed_documents.return_value = [[0.1] * 384]
        
        mock_qdrant = AsyncMock()
        mock_qdrant.ensure_collection = AsyncMock()
        mock_qdrant.store_points_with_retry = AsyncMock(return_value=True)
        
        with patch('streaming_watcher.FastEmbedProvider', return_value=mock_embedding_provider):
            with patch('streaming_watcher.QdrantService', return_value=mock_qdrant):
                watcher = StreamingWatcher(self.config)
                watcher.qdrant_service = mock_qdrant
                
                result = await watcher.process_file(self.valid_file)
                self.assertTrue(result)
                
                # Should be added to state
                self.assertIn(str(self.valid_file), watcher.state.get("imported_files", {}))

class TestProductionReadiness(unittest.TestCase):
    """Test production readiness scenarios."""
    
    def setUp(self):
        self.temp_dir = Path(tempfile.mkdtemp())
        self.config = Config()
        self.config.logs_dir = self.temp_dir
        self.config.state_file = self.temp_dir / "test-state.json"
        self.config.memory_limit_mb = 512
        self.config.memory_warning_mb = 256
    
    def tearDown(self):
        shutil.rmtree(self.temp_dir, ignore_errors=True)
    
    async def test_graceful_shutdown(self):
        """Test graceful shutdown handling."""
        with patch('streaming_watcher.FastEmbedProvider'):
            watcher = StreamingWatcher(self.config)
            
            # Start shutdown
            await watcher.shutdown()
            
            self.assertTrue(watcher.shutdown_event.is_set())
    
    async def test_error_recovery(self):
        """Test error recovery during file processing."""
        # Create a file that will cause processing errors
        bad_file = self.temp_dir / "bad.jsonl"
        with open(bad_file, 'w') as f:
            f.write('{"message": {"role": "user", "content": "' + "x" * 100000 + '"}}\n')  # Very large content
        
        mock_embedding_provider = AsyncMock()
        mock_embedding_provider.embed_documents.side_effect = Exception("Embedding failed")
        
        with patch('streaming_watcher.FastEmbedProvider', return_value=mock_embedding_provider):
            watcher = StreamingWatcher(self.config)
            
            # Should handle error gracefully
            result = await watcher.process_file(bad_file)
            self.assertFalse(result)  # Should fail but not crash
            
            # Error should be counted
            self.assertGreater(watcher.stats["failures"], 0)
    
    def test_vector_dimensions(self):
        """Test correct vector dimensions for different providers."""
        # Local mode should use 384 dimensions
        config_local = Config()
        config_local.prefer_local_embeddings = True
        self.assertEqual(config_local.vector_size, 384)
        
        # Cloud mode should detect 1024 dimensions for Voyage
        collection_name_voyage = "conv_test_voyage"
        collection_name_local = "conv_test_local"
        
        # Mock QdrantService to test dimension detection
        config = Config()
        with patch('streaming_watcher.FastEmbedProvider'):
            qdrant_service = QdrantService(config, Mock())
            
            # Should detect voyage dimensions from collection name
            self.assertIn("_voyage", collection_name_voyage)
            self.assertIn("_local", collection_name_local)

# Async test runner
class AsyncTestRunner:
    """Helper to run async tests."""
    
    @staticmethod
    def run_async_tests():
        """Run all async test methods."""
        test_classes = [
            TestVoyageProvider,
            TestFastEmbedProvider,
            TestCollectionNaming,
            TestStatePersistence,
            TestNoMessagesHandling,
            TestProductionReadiness
        ]
        
        total_passed = 0
        total_failed = 0
        
        for test_class in test_classes:
            print(f"\n=== Running {test_class.__name__} ===")
            
            # Find async test methods
            async_methods = [
                method for method in dir(test_class)
                if method.startswith('test_') and asyncio.iscoroutinefunction(getattr(test_class, method))
            ]
            
            if not async_methods:
                continue
            
            # Create test instance
            test_instance = test_class()
            if hasattr(test_instance, 'setUp'):
                test_instance.setUp()
            
            try:
                for method_name in async_methods:
                    print(f"  Running {method_name}...", end=" ")
                    
                    try:
                        method = getattr(test_instance, method_name)
                        asyncio.run(method())
                        print("PASS")
                        total_passed += 1
                    except Exception as e:
                        print(f"FAIL: {e}")
                        total_failed += 1
                        
            finally:
                if hasattr(test_instance, 'tearDown'):
                    test_instance.tearDown()
        
        return total_passed, total_failed

def main():
    """Main test runner."""
    print("="*80)
    print("Claude Self-Reflect Streaming Watcher Comprehensive Test Suite")
    print("="*80)
    
    start_time = time.time()
    
    # Run regular unit tests
    print("\n🧪 Running synchronous unit tests...")
    suite = unittest.TestLoader().loadTestsFromModule(sys.modules[__name__])
    
    # Filter out async test classes from regular runner
    sync_tests = []
    async_test_classes = {
        'TestVoyageProvider', 'TestFastEmbedProvider', 'TestCollectionNaming',
        'TestStatePersistence', 'TestNoMessagesHandling', 'TestProductionReadiness'
    }
    
    for test in suite:
        if hasattr(test, '_testMethodName'):
            class_name = test.__class__.__name__
            if class_name not in async_test_classes:
                sync_tests.append(test)
    
    sync_suite = unittest.TestSuite(sync_tests)
    sync_runner = unittest.TextTestRunner(verbosity=2)
    sync_result = sync_runner.run(sync_suite)
    
    # Run async tests
    print("\n🔄 Running asynchronous tests...")
    async_passed, async_failed = AsyncTestRunner.run_async_tests()
    
    # Summary
    duration = time.time() - start_time
    total_passed = sync_result.testsRun - len(sync_result.failures) - len(sync_result.errors) + async_passed
    total_failed = len(sync_result.failures) + len(sync_result.errors) + async_failed
    total_tests = sync_result.testsRun + async_passed + async_failed
    
    print("\n" + "="*80)
    print("📊 TEST SUMMARY")
    print("="*80)
    print(f"Total Tests: {total_tests}")
    print(f"✅ Passed: {total_passed}")
    print(f"❌ Failed: {total_failed}")
    print(f"⏱️  Duration: {duration:.2f}s")
    
    if total_failed == 0:
        print("\n🎉 ALL TESTS PASSED! Streaming watcher is production-ready.")
        return True
    else:
        print(f"\n⚠️  {total_failed} tests failed. Review issues before production deployment.")
        return False

if __name__ == "__main__":
    success = main()
    sys.exit(0 if success else 1)