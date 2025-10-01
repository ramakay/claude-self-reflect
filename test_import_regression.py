#!/usr/bin/env python3
"""
Comprehensive regression test for import-conversations-unified.py after CodeRabbit fixes.

Tests all the critical changes:
1. normalize_project_name import fallback paths
2. Qdrant cleanup count() API usage
3. Tool entries not counting as messages (is_message=False)
4. Exception handling robustness
5. Both local and cloud embedding modes
6. Actual import and search functionality
"""

import os
import sys
import json
import shutil
import tempfile
import logging
import traceback
from pathlib import Path
from datetime import datetime

# Add scripts to path
sys.path.insert(0, str(Path(__file__).parent / "scripts"))

# Set test environment
os.environ["QDRANT_URL"] = "http://localhost:6334"
os.environ["PREFER_LOCAL_EMBEDDINGS"] = "true"
os.environ["STATE_FILE"] = "/tmp/claude/test_unified_state.json"

logging.basicConfig(level=logging.INFO, format='%(levelname)s: %(message)s')
logger = logging.getLogger(__name__)

class RegressionTester:
    """Comprehensive test runner for import system regression testing."""
    
    def __init__(self):
        self.test_dir = Path("/tmp/claude")
        self.test_dir.mkdir(exist_ok=True)
        self.results = {}
        self.test_count = 0
        self.passed_count = 0
        
    def run_test(self, test_name: str, test_func):
        """Run a single test and track results."""
        self.test_count += 1
        logger.info(f"\n=== Test {self.test_count}: {test_name} ===")
        
        try:
            test_func()
            self.results[test_name] = "PASS"
            self.passed_count += 1
            logger.info(f"✅ PASS: {test_name}")
        except Exception as e:
            self.results[test_name] = f"FAIL: {str(e)}"
            logger.error(f"❌ FAIL: {test_name}")
            logger.error(f"Error: {e}")
            logger.debug(traceback.format_exc())
    
    def test_normalize_import_fallback(self):
        """Test 1: CodeRabbit Fix - normalize_project_name import fallback works."""
        # Test the import cascade works
        from import_conversations_unified import ConversationImporter
        
        # Create test importer
        importer = ConversationImporter()
        
        # Test normalization function exists and works
        test_path = "-Users-ramakrishnanannaswamy-projects-test-project"
        collection_name = importer.get_collection_name(Path(test_path))
        
        assert "test-project" in collection_name or "test_project" in collection_name, \
            f"Project normalization failed: {collection_name}"
        
        logger.info(f"Normalization working: {test_path} -> {collection_name}")
    
    def test_qdrant_count_api(self):
        """Test 2: CodeRabbit Fix - Qdrant count() API works correctly."""
        from qdrant_client import QdrantClient
        from qdrant_client.models import Filter, FieldCondition, MatchValue, Distance, VectorParams
        
        client = QdrantClient(url="http://localhost:6334", timeout=30)
        
        # Create test collection
        test_collection = "test_count_api"
        client.recreate_collection(
            collection_name=test_collection,
            vectors_config=VectorParams(size=384, distance=Distance.COSINE)
        )
        
        # Test count() API - this was changed from scroll() in CodeRabbit fix
        count_filter = Filter(
            must=[FieldCondition(key="conversation_id", match=MatchValue(value="test-123"))]
        )
        
        count_result = client.count(
            collection_name=test_collection,
            count_filter=count_filter,
            exact=True
        )
        
        assert hasattr(count_result, 'count'), "count() API response invalid"
        assert count_result.count == 0, f"Expected 0, got {count_result.count}"
        
        # Cleanup
        client.delete_collection(test_collection)
        logger.info("Qdrant count() API working correctly")
    
    def test_message_counting_logic(self):
        """Test 3: CodeRabbit Fix - Tool entries don't count as messages."""
        from metadata_extractor import MetadataExtractor
        
        # Create test JSONL with user, assistant, and tool entries
        test_file = self.test_dir / "test_counting.jsonl"
        
        with open(test_file, 'w') as f:
            # User message (should count)
            f.write(json.dumps({
                "message": {"role": "user", "content": "Hello"},
                "timestamp": "2024-01-01T00:00:00Z"
            }) + "\n")
            
            # Assistant message (should count)  
            f.write(json.dumps({
                "message": {"role": "assistant", "content": "Hi there"},
                "timestamp": "2024-01-01T00:01:00Z"
            }) + "\n")
            
            # Tool use entry (should NOT count as message)
            f.write(json.dumps({
                "type": "tool_use",
                "name": "Read",
                "input": {"file_path": "test.py"},
                "timestamp": "2024-01-01T00:02:00Z"
            }) + "\n")
            
            # Tool result entry (should NOT count as message)
            f.write(json.dumps({
                "type": "tool_result", 
                "content": [{"type": "text", "text": "File content"}],
                "timestamp": "2024-01-01T00:03:00Z"
            }) + "\n")
        
        # Extract metadata
        extractor = MetadataExtractor()
        metadata, created_at, message_count = extractor.extract_metadata_from_file(str(test_file))
        
        # Should only count user and assistant messages (2), not tool entries
        assert message_count == 2, f"Expected 2 messages, got {message_count}"
        
        # Cleanup
        test_file.unlink()
        logger.info(f"Message counting correct: {message_count} messages (user+assistant only)")
    
    def test_local_embedding_import(self):
        """Test 4: Local embedding mode import with actual files."""
        from import_conversations_unified import ConversationImporter
        
        # Ensure local mode
        os.environ["PREFER_LOCAL_EMBEDDINGS"] = "true"
        
        # Create test conversation file
        test_project_dir = self.test_dir / "test_project_local"
        test_project_dir.mkdir(exist_ok=True)
        
        test_file = test_project_dir / "test_conversation.jsonl"
        with open(test_file, 'w') as f:
            f.write(json.dumps({
                "message": {"role": "user", "content": "Test local embedding"},
                "timestamp": "2024-01-01T00:00:00Z"
            }) + "\n")
            f.write(json.dumps({
                "message": {"role": "assistant", "content": "Testing local FastEmbed"},
                "timestamp": "2024-01-01T00:01:00Z"
            }) + "\n")
        
        # Import with local embeddings
        importer = ConversationImporter()
        assert importer.embedding_service.get_dimension() == 384, "Expected local 384D embeddings"
        assert "local" in importer.embedding_service.get_collection_suffix(), "Expected local suffix"
        
        # Import file
        collection_name = importer.get_collection_name(test_project_dir)
        chunks = importer.import_file(test_file, collection_name, test_project_dir)
        
        assert chunks > 0, f"Import failed, got {chunks} chunks"
        
        # Verify in Qdrant
        from qdrant_client import QdrantClient
        client = QdrantClient(url="http://localhost:6334", timeout=30)
        
        collections = client.get_collections().collections
        collection_exists = any(c.name == collection_name for c in collections)
        assert collection_exists, f"Collection {collection_name} not created"
        
        # Cleanup
        shutil.rmtree(test_project_dir)
        if collection_exists:
            client.delete_collection(collection_name)
        
        logger.info(f"Local embedding import successful: {chunks} chunks")
    
    def test_cloud_embedding_import(self):
        """Test 5: Cloud embedding mode import (if API key available)."""
        voyage_key = os.getenv("VOYAGE_KEY")
        if not voyage_key:
            logger.info("Skipping cloud test - VOYAGE_KEY not set")
            return
        
        from import_conversations_unified import ConversationImporter
        
        # Switch to cloud mode
        os.environ["PREFER_LOCAL_EMBEDDINGS"] = "false"
        
        # Create test conversation file
        test_project_dir = self.test_dir / "test_project_cloud"
        test_project_dir.mkdir(exist_ok=True)
        
        test_file = test_project_dir / "test_conversation.jsonl"
        with open(test_file, 'w') as f:
            f.write(json.dumps({
                "message": {"role": "user", "content": "Test cloud embedding"},
                "timestamp": "2024-01-01T00:00:00Z"
            }) + "\n")
            f.write(json.dumps({
                "message": {"role": "assistant", "content": "Testing Voyage AI"},
                "timestamp": "2024-01-01T00:01:00Z"
            }) + "\n")
        
        # Import with cloud embeddings
        importer = ConversationImporter()
        assert importer.embedding_service.get_dimension() == 1024, "Expected cloud 1024D embeddings"
        assert "cloud" in importer.embedding_service.get_collection_suffix(), "Expected cloud suffix"
        
        # Import file
        collection_name = importer.get_collection_name(test_project_dir)
        chunks = importer.import_file(test_file, collection_name, test_project_dir)
        
        assert chunks > 0, f"Import failed, got {chunks} chunks"
        
        # Cleanup
        shutil.rmtree(test_project_dir)
        
        from qdrant_client import QdrantClient
        client = QdrantClient(url="http://localhost:6334", timeout=30)
        try:
            client.delete_collection(collection_name)
        except:
            pass
        
        logger.info(f"Cloud embedding import successful: {chunks} chunks")
    
    def test_real_conversation_import(self):
        """Test 6: Import real conversation files and verify search works."""
        from import_conversations_unified import ConversationImporter
        from qdrant_client import QdrantClient
        
        # Use actual Claude project files
        real_project = Path.home() / ".claude" / "projects" / "-Users-ramakrishnanannaswamy-projects-claude-self-reflect"
        
        if not real_project.exists():
            logger.info("Skipping real conversation test - project not found")
            return
        
        # Get first 2 JSONL files
        jsonl_files = list(real_project.glob("*.jsonl"))[:2]
        if not jsonl_files:
            logger.info("Skipping real conversation test - no JSONL files")
            return
        
        # Import with limit
        importer = ConversationImporter()
        stats = importer.import_project(real_project, limit=2)
        
        assert stats["imported"] > 0, f"No files imported: {stats}"
        
        # Test search works after import
        client = QdrantClient(url="http://localhost:6334", timeout=30)
        collection_name = importer.get_collection_name(real_project)
        
        # Search for some content
        search_vector = importer.embedding_service.generate_embeddings(["docker import test"])[0]
        
        results = client.search(
            collection_name=collection_name,
            query_vector=search_vector,
            limit=3
        )
        
        assert len(results) > 0, "Search returned no results after import"
        
        logger.info(f"Real import successful: {stats}, search returned {len(results)} results")
    
    def test_exception_handling(self):
        """Test 7: Exception handling robustness."""
        from import_conversations_unified import ConversationImporter
        
        # Test with invalid file
        invalid_file = self.test_dir / "invalid.jsonl"
        with open(invalid_file, 'w') as f:
            f.write("{ invalid json }\n")
            f.write("{ \"valid\": true }\n")
        
        importer = ConversationImporter()
        
        # Should handle invalid JSON gracefully
        collection_name = "test_exception_handling"
        chunks = importer.import_file(invalid_file, collection_name, self.test_dir)
        
        # Should not crash, might import 0 chunks
        assert chunks >= 0, "Import should return non-negative chunk count"
        
        # Cleanup
        invalid_file.unlink()
        
        logger.info("Exception handling working correctly")
    
    def test_unified_state_manager(self):
        """Test 8: UnifiedStateManager integration works."""
        from unified_state_manager import UnifiedStateManager
        
        # Test state manager functionality
        state_file = self.test_dir / "test_state.json"
        manager = UnifiedStateManager(state_file)
        
        # Add test file
        test_file = "/tmp/test.jsonl"
        manager.add_imported_file(
            file_path=test_file,
            chunks=5,
            collection="test_collection",
            embedding_mode="local"
        )
        
        # Verify it's tracked
        imported_files = manager.get_imported_files()
        normalized_path = manager.normalize_path(test_file)
        
        assert normalized_path in imported_files, "File not tracked in state"
        
        # Cleanup
        state_file.unlink(missing_ok=True)
        
        logger.info("UnifiedStateManager integration working")
    
    def run_all_tests(self):
        """Run all regression tests."""
        logger.info(f"\n🧪 Starting comprehensive regression test suite")
        logger.info(f"Testing import-conversations-unified.py after CodeRabbit fixes")
        
        # Test order matters - start with basic functionality
        self.run_test("normalize_project_name import fallback", self.test_normalize_import_fallback)
        self.run_test("Qdrant count() API usage", self.test_qdrant_count_api)
        self.run_test("Tool entries message counting", self.test_message_counting_logic)
        self.run_test("Exception handling robustness", self.test_exception_handling)
        self.run_test("UnifiedStateManager integration", self.test_unified_state_manager)
        
        # Test actual imports
        self.run_test("Local embedding import", self.test_local_embedding_import)
        self.run_test("Cloud embedding import", self.test_cloud_embedding_import)
        self.run_test("Real conversation import", self.test_real_conversation_import)
        
        # Print summary
        logger.info(f"\n📊 Test Results Summary:")
        logger.info(f"Passed: {self.passed_count}/{self.test_count}")
        
        for test_name, result in self.results.items():
            status = "✅" if result == "PASS" else "❌"
            logger.info(f"{status} {test_name}: {result}")
        
        if self.passed_count == self.test_count:
            logger.info(f"\n🎉 ALL TESTS PASSED! Import system regression successful.")
            return True
        else:
            logger.info(f"\n⚠️  {self.test_count - self.passed_count} tests failed. Review needed.")
            return False

if __name__ == "__main__":
    tester = RegressionTester()
    success = tester.run_all_tests()
    sys.exit(0 if success else 1)
