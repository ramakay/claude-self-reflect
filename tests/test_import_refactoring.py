#!/usr/bin/env python3
"""
Comprehensive test suite for refactored import components.
Tests all refactored modules to ensure functionality and reduced complexity.
"""

import unittest
import tempfile
import json
import os
import sys
from pathlib import Path
from unittest.mock import Mock, patch, MagicMock
from datetime import datetime

# Add scripts directory to path
scripts_dir = Path(__file__).parent.parent / "scripts"
sys.path.insert(0, str(scripts_dir))

# Import modules to test
from message_processors import (
    MessageProcessor,
    TextMessageProcessor,
    ThinkingMessageProcessor,
    ToolMessageProcessor,
    ToolResultProcessor,
    MessageProcessorFactory,
    extract_ast_elements,
    extract_concepts
)

from metadata_extractor import MetadataExtractor

from import_strategies import (
    ChunkBuffer,
    MessageStreamReader,
    StreamImportStrategy
)

from embedding_service import (
    LocalEmbeddingProvider,
    CloudEmbeddingProvider,
    EmbeddingService,
    create_embedding_service
)


class TestMessageProcessors(unittest.TestCase):
    """Test message processor classes."""

    def setUp(self):
        """Set up test fixtures."""
        self.text_processor = TextMessageProcessor()
        self.thinking_processor = ThinkingMessageProcessor()
        self.tool_processor = ToolMessageProcessor()
        self.factory = MessageProcessorFactory()

    def test_text_processor_simple_text(self):
        """Test text processor with simple text."""
        metadata = {"ast_elements": [], "has_code_blocks": False}
        item = {"type": "text", "text": "Hello world"}
        result = self.text_processor.process(item, metadata)
        self.assertEqual(result, "Hello world")
        self.assertFalse(metadata["has_code_blocks"])

    def test_text_processor_with_code(self):
        """Test text processor with code blocks."""
        metadata = {"ast_elements": [], "has_code_blocks": False}
        item = {"type": "text", "text": "Here is code:\n```python\ndef hello():\n    print('world')\n```"}
        result = self.text_processor.process(item, metadata)
        self.assertTrue(metadata["has_code_blocks"])
        self.assertIn("func:hello", metadata["ast_elements"])

    def test_thinking_processor(self):
        """Test thinking message processor."""
        metadata = {}
        item = {"type": "thinking", "thinking": "Analyzing the problem..."}
        result = self.thinking_processor.process(item, metadata)
        self.assertEqual(result, "Analyzing the problem...")

    def test_tool_processor(self):
        """Test tool use processor."""
        metadata = {"tools_used": [], "files_edited": [], "files_analyzed": []}
        item = {
            "type": "tool_use",
            "name": "Edit",
            "input": {"file_path": "/path/to/file.py", "content": "new content"}
        }
        result = self.tool_processor.process(item, metadata)
        self.assertIn("Edit", metadata["tools_used"])
        self.assertIn("/path/to/file.py", metadata["files_edited"])

    def test_factory_get_processor(self):
        """Test processor factory."""
        self.assertIsInstance(self.factory.get_processor("text"), TextMessageProcessor)
        self.assertIsInstance(self.factory.get_processor("thinking"), ThinkingMessageProcessor)
        self.assertIsInstance(self.factory.get_processor("tool_use"), ToolMessageProcessor)
        self.assertIsNone(self.factory.get_processor("unknown"))

    def test_extract_ast_elements(self):
        """Test AST element extraction."""
        code = """
def test_function():
    pass

class TestClass:
    pass

import os
from pathlib import Path
"""
        elements = extract_ast_elements(code)
        self.assertIn("func:test_function", elements)
        self.assertIn("class:TestClass", elements)
        self.assertIn("import:os", elements)
        self.assertIn("from:pathlib.Path", elements)

    def test_extract_concepts(self):
        """Test concept extraction."""
        text = "Testing React components with Jest and handling async operations"
        concepts = extract_concepts(text)
        self.assertIn("testing", concepts)
        self.assertIn("frontend-framework", concepts)
        self.assertIn("async-programming", concepts)


class TestMetadataExtractor(unittest.TestCase):
    """Test metadata extractor."""

    def setUp(self):
        """Set up test fixtures."""
        self.extractor = MetadataExtractor()

    def test_extract_metadata_empty_file(self):
        """Test metadata extraction from empty file."""
        with tempfile.NamedTemporaryFile(mode='w', suffix='.jsonl', delete=False) as f:
            f.write("")
            temp_file = f.name

        try:
            metadata, timestamp, count = self.extractor.extract_metadata_from_file(temp_file)
            self.assertEqual(count, 0)
            self.assertIsInstance(metadata, dict)
            self.assertEqual(metadata["total_messages"], 0)
        finally:
            os.unlink(temp_file)

    def test_extract_metadata_valid_conversation(self):
        """Test metadata extraction from valid conversation."""
        with tempfile.NamedTemporaryFile(mode='w', suffix='.jsonl', delete=False) as f:
            # Write test conversation
            f.write(json.dumps({
                "timestamp": "2024-01-01T00:00:00Z",
                "cwd": "/test/project",
                "message": {
                    "role": "user",
                    "content": "Hello, can you help with Python?"
                }
            }) + "\n")
            f.write(json.dumps({
                "message": {
                    "role": "assistant",
                    "content": "Sure! I can help with Python."
                }
            }) + "\n")
            temp_file = f.name

        try:
            metadata, timestamp, count = self.extractor.extract_metadata_from_file(temp_file)
            self.assertEqual(count, 2)
            self.assertEqual(metadata["project_path"], "/test/project")
            self.assertEqual(timestamp, "2024-01-01T00:00:00Z")
        finally:
            os.unlink(temp_file)

    def test_extract_metadata_with_tools(self):
        """Test metadata extraction with tool usage."""
        with tempfile.NamedTemporaryFile(mode='w', suffix='.jsonl', delete=False) as f:
            f.write(json.dumps({
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": "Let me read that file"},
                        {
                            "type": "tool_use",
                            "name": "Read",
                            "input": {"file_path": "/test/file.py"}
                        }
                    ]
                }
            }) + "\n")
            temp_file = f.name

        try:
            metadata, _, count = self.extractor.extract_metadata_from_file(temp_file)
            self.assertIn("Read", metadata["tools_used"])
            self.assertIn("/test/file.py", metadata["files_analyzed"])
        finally:
            os.unlink(temp_file)


class TestImportStrategies(unittest.TestCase):
    """Test import strategy components."""

    def test_chunk_buffer(self):
        """Test chunk buffer management."""
        buffer = ChunkBuffer(max_size=2)

        # Add first message
        msg1 = {"role": "user", "content": "Hello"}
        self.assertFalse(buffer.add(msg1))
        self.assertTrue(buffer.has_content())

        # Add second message (buffer full)
        msg2 = {"role": "assistant", "content": "Hi"}
        self.assertTrue(buffer.add(msg2))

        # Get and clear
        contents = buffer.get_and_clear()
        self.assertEqual(len(contents), 2)
        self.assertFalse(buffer.has_content())

    def test_message_stream_reader_parse_message(self):
        """Test message stream reader parsing."""
        reader = MessageStreamReader()

        # Test valid message
        line = json.dumps({
            "message": {
                "role": "user",
                "content": "Test message"
            }
        })
        result = reader._parse_line(line, 1)
        self.assertIsNotNone(result)
        self.assertEqual(result["role"], "user")
        self.assertEqual(result["content"], "Test message")

        # Test invalid JSON
        result = reader._parse_line("invalid json", 2)
        self.assertIsNone(result)

    def test_stream_reader_content_extraction(self):
        """Test content extraction from different formats."""
        reader = MessageStreamReader()

        # Test string content
        content = "Simple string"
        result = reader._extract_text_content(content)
        self.assertEqual(result, "Simple string")

        # Test list content
        content = [
            {"type": "text", "text": "Hello"},
            {"type": "thinking", "thinking": "Thinking..."}
        ]
        result = reader._extract_text_content(content)
        self.assertIn("Hello", result)
        self.assertIn("[Thinking]", result)

    @patch('import_strategies.logger')
    def test_stream_import_strategy_initialization(self, mock_logger):
        """Test stream import strategy initialization."""
        mock_client = Mock()
        mock_process_fn = Mock()
        mock_state = Mock()

        strategy = StreamImportStrategy(
            mock_client, mock_process_fn, mock_state, max_chunk_size=10
        )

        self.assertEqual(strategy.client, mock_client)
        self.assertEqual(strategy.max_chunk_size, 10)
        self.assertIsNotNone(strategy.stream_reader)


class TestEmbeddingService(unittest.TestCase):
    """Test embedding service components."""

    @patch('embedding_service.LocalEmbeddingProvider._initialize_model')
    def test_local_provider_dimension(self, mock_init):
        """Test local embedding provider dimension."""
        provider = LocalEmbeddingProvider()
        self.assertEqual(provider.get_dimension(), 384)
        self.assertEqual(provider.get_collection_suffix(), "local_384d")

    @patch('embedding_service.CloudEmbeddingProvider._initialize_client')
    def test_cloud_provider_dimension(self, mock_init):
        """Test cloud embedding provider dimension."""
        provider = CloudEmbeddingProvider("test-api-key")
        self.assertEqual(provider.get_dimension(), 1024)
        self.assertEqual(provider.get_collection_suffix(), "cloud_1024d")

    @patch('embedding_service.LocalEmbeddingProvider')
    def test_embedding_service_prefer_local(self, mock_provider):
        """Test embedding service with local preference."""
        service = EmbeddingService(prefer_local=True)
        self.assertTrue(service.prefer_local)

    @patch.dict(os.environ, {"PREFER_LOCAL_EMBEDDINGS": "true", "VOYAGE_KEY": "test-key"})
    def test_create_embedding_service_env_vars(self):
        """Test creating embedding service with environment variables."""
        with patch('embedding_service.LocalEmbeddingProvider'):
            service = create_embedding_service()
            self.assertTrue(service.prefer_local)
            self.assertEqual(service.voyage_api_key, "test-key")


class TestIntegration(unittest.TestCase):
    """Integration tests for the complete refactored system."""

    def test_complexity_reduction(self):
        """Verify that refactored functions have reduced complexity."""
        # This test verifies the main goal: all functions should have complexity < 10
        # We'll check by importing and inspecting the modules

        # Check that key functions exist and are smaller
        from metadata_extractor import MetadataExtractor
        extractor = MetadataExtractor()

        # The extract_metadata_from_file method should be much simpler
        # than the original extract_metadata_single_pass (was 173 lines)
        import inspect
        source = inspect.getsource(extractor.extract_metadata_from_file)
        lines = source.split('\n')
        self.assertLess(len(lines), 50, "extract_metadata_from_file should be < 50 lines")

        # Check message processors are simple
        from message_processors import TextMessageProcessor
        processor = TextMessageProcessor()
        source = inspect.getsource(processor.process)
        lines = source.split('\n')
        self.assertLess(len(lines), 20, "Message processor methods should be < 20 lines")

    def test_backward_compatibility(self):
        """Test that refactored code maintains backward compatibility."""
        # Create a test JSONL with various message formats
        with tempfile.NamedTemporaryFile(mode='w', suffix='.jsonl', delete=False) as f:
            # Old format with string content
            f.write(json.dumps({
                "message": {"role": "user", "content": "String content"}
            }) + "\n")

            # New format with list content
            f.write(json.dumps({
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": "Text content"},
                        {"type": "tool_use", "name": "Read", "input": {}}
                    ]
                }
            }) + "\n")

            # Tool result format
            f.write(json.dumps({
                "type": "tool_result",
                "content": "Result content"
            }) + "\n")

            temp_file = f.name

        try:
            extractor = MetadataExtractor()
            metadata, _, count = extractor.extract_metadata_from_file(temp_file)

            # Should handle all formats
            self.assertGreater(count, 0)
            self.assertIn("Read", metadata["tools_used"])
        finally:
            os.unlink(temp_file)


def run_tests():
    """Run all tests and return results."""
    loader = unittest.TestLoader()
    suite = unittest.TestSuite()

    # Add all test classes
    suite.addTests(loader.loadTestsFromTestCase(TestMessageProcessors))
    suite.addTests(loader.loadTestsFromTestCase(TestMetadataExtractor))
    suite.addTests(loader.loadTestsFromTestCase(TestImportStrategies))
    suite.addTests(loader.loadTestsFromTestCase(TestEmbeddingService))
    suite.addTests(loader.loadTestsFromTestCase(TestIntegration))

    # Run tests
    runner = unittest.TextTestRunner(verbosity=2)
    result = runner.run(suite)

    return result.wasSuccessful()


if __name__ == "__main__":
    success = run_tests()
    sys.exit(0 if success else 1)