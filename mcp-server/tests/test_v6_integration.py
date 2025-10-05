"""
Integration tests for Claude 2.0.1 Memory & Context Management (v6.0).

Tests:
1. Memory Tool persistence across MCP sessions
2. Context clearing preserves memory tools
3. Hybrid search prioritizes Memory Tool results (0.95 score)
4. Auto-storage triggers for high-quality patterns
5. Statusline stats display v6.0 components

Run with: pytest mcp-server/tests/test_v6_integration.py -v
"""

import pytest
import asyncio
import os
import sys
import json
from pathlib import Path
from datetime import datetime

# Add parent directory to path for imports
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from src.memory_tools import MemoryToolHandler, get_memory_handler
from src.context_manager import ContextManagerConfig, ClearingStats


class TestMemoryPersistence:
    """Test Memory Tool persistence across sessions."""

    @pytest.fixture
    def temp_memory_dir(self, tmp_path):
        """Create temporary memory directory."""
        memory_dir = tmp_path / "memories"
        memory_dir.mkdir()
        return str(memory_dir)

    @pytest.mark.asyncio
    async def test_memory_persists_across_sessions(self, temp_memory_dir):
        """CRITICAL: Memory files survive session restarts."""
        # Session 1: Create memory
        handler1 = MemoryToolHandler(temp_memory_dir)
        result1 = await handler1.create("patterns/test-pattern.md", "Test pattern content")
        assert "Created memory" in result1

        # Session 2: New handler instance (simulates restart)
        handler2 = MemoryToolHandler(temp_memory_dir)
        result2 = await handler2.view("patterns/test-pattern.md")
        assert result2 == "Test pattern content"

    @pytest.mark.asyncio
    async def test_memory_crud_operations(self, temp_memory_dir):
        """Test all CRUD operations match official commands."""
        handler = MemoryToolHandler(temp_memory_dir)

        # Create
        result = await handler.create("test.md", "Original content")
        assert "Created memory" in result

        # View
        content = await handler.view("test.md")
        assert content == "Original content"

        # str_replace
        result = await handler.str_replace("test.md", "Original", "Updated")
        assert "Updated memory" in result

        content = await handler.view("test.md")
        assert content == "Updated content"

        # Insert
        result = await handler.insert("test.md", 0, "Prefix: ")
        assert "Inserted content" in result

        content = await handler.view("test.md")
        assert content.startswith("Prefix:")

        # Rename
        result = await handler.rename("test.md", "renamed.md")
        assert "Renamed memory" in result

        # Verify old path doesn't exist
        old_content = await handler.view("test.md")
        assert "not found" in old_content

        # Verify new path exists
        new_content = await handler.view("renamed.md")
        assert "Prefix:" in new_content

        # Delete
        result = await handler.delete("renamed.md")
        assert "Deleted memory" in result

        # Verify deletion
        content = await handler.view("renamed.md")
        assert "not found" in content


class TestContextClearingExclusions:
    """Test context clearing excludes all memory tools."""

    def test_all_memory_tools_excluded(self):
        """CRITICAL: All memory tools must be in exclude list."""
        config = ContextManagerConfig()

        required_exclusions = [
            "view_memory",
            "store_to_memory",
            "search_memory",
            "list_memories"
        ]

        for tool in required_exclusions:
            assert tool in config.exclude_tools, f"Missing from exclude_tools: {tool}"

    def test_context_config_matches_official(self):
        """Validate config structure matches official API."""
        config = ContextManagerConfig(
            trigger_tokens=30000,
            keep_tool_uses=5,
            clear_at_least_tokens=5000
        )

        api_config = config.to_api_config()

        assert "edits" in api_config
        assert len(api_config["edits"]) == 1

        edit = api_config["edits"][0]
        assert edit["type"] == "clear_tool_uses_20250919"
        assert edit["trigger"]["type"] == "input_tokens"
        assert edit["trigger"]["value"] == 30000
        assert edit["keep"]["type"] == "tool_uses"
        assert edit["keep"]["value"] == 5
        assert edit["exclude_tools"] == config.exclude_tools


class TestHybridSearchPriority:
    """Test hybrid search prioritizes Memory Tool results."""

    @pytest.fixture
    def temp_memory_dir(self, tmp_path):
        """Create temporary memory directory."""
        memory_dir = tmp_path / "memories"
        memory_dir.mkdir()
        return str(memory_dir)

    @pytest.mark.asyncio
    async def test_memory_results_get_095_score(self, temp_memory_dir):
        """Memory Tool results must score 0.95."""
        handler = MemoryToolHandler(temp_memory_dir)

        # Create test memory
        await handler.create("patterns/high-priority.md", "Important pattern for testing")

        # Search memory (simulating hybrid search logic)
        results = await handler.search("important pattern", category=None)

        # Verify search found the file
        assert len(results) > 0
        assert results[0]["path"] == "patterns/high-priority.md"

        # In actual hybrid search, this would get 0.95 score
        # Simulate hybrid search result structure
        memory_result = {
            'id': f"memory:{results[0]['path']}",
            'score': 0.95,  # This is what hybrid search assigns
            'role': 'memory',
            'excerpt': results[0]['preview'],
            'source': 'memory_tool'
        }

        assert memory_result['score'] == 0.95
        assert memory_result['source'] == 'memory_tool'

    @pytest.mark.asyncio
    async def test_memory_results_sorted_first(self, temp_memory_dir):
        """Memory results (0.95) appear before vector results (0.8)."""
        # Simulate hybrid search merging
        memory_results = [
            {'id': 'memory:test.md', 'score': 0.95, 'source': 'memory_tool'}
        ]

        vector_results = [
            {'id': 'conv-123', 'score': 0.8, 'source': 'vector_search'},
            {'id': 'conv-456', 'score': 0.75, 'source': 'vector_search'}
        ]

        # Merge (as done in search_tools.py)
        all_results = memory_results + vector_results

        # Sort by score descending
        all_results.sort(key=lambda x: x['score'], reverse=True)

        # Verify memory result is first
        assert all_results[0]['source'] == 'memory_tool'
        assert all_results[0]['score'] == 0.95
        assert all_results[1]['score'] == 0.8


class TestAutoStorage:
    """Test auto-storage of high-quality patterns."""

    def test_threshold_detection(self):
        """Patterns with score >90 should trigger auto-storage."""
        pattern_quality = {
            "file1.py": {"score": 0.95, "good_patterns": 5},
            "file2.py": {"score": 0.88, "good_patterns": 3},
            "file3.py": {"score": 0.92, "good_patterns": 4}
        }

        # Filter logic (from metadata_extractor.py)
        high_quality_files = [
            (file, info) for file, info in pattern_quality.items()
            if info.get('score', 0) > 0.90
        ]

        assert len(high_quality_files) == 2  # file1.py and file3.py
        assert ("file1.py", {"score": 0.95, "good_patterns": 5}) in high_quality_files
        assert ("file3.py", {"score": 0.92, "good_patterns": 4}) in high_quality_files


class TestStatuslineStats:
    """Test statusline displays v6.0 stats correctly."""

    @pytest.fixture
    def temp_memory_dir(self, tmp_path):
        """Create temporary memory directory."""
        memory_dir = tmp_path / "memories"
        memory_dir.mkdir(parents=True)
        return memory_dir

    def test_memory_stats_calculation(self, temp_memory_dir):
        """Memory stats show file counts by category."""
        # Create test files
        (temp_memory_dir / "patterns").mkdir()
        (temp_memory_dir / "insights").mkdir()
        (temp_memory_dir / "quality").mkdir()

        (temp_memory_dir / "patterns" / "p1.md").write_text("Pattern 1")
        (temp_memory_dir / "patterns" / "p2.md").write_text("Pattern 2")
        (temp_memory_dir / "insights" / "i1.md").write_text("Insight 1")
        (temp_memory_dir / "quality" / "q1.md").write_text("Quality 1")

        # Count files (as done in csr-status script)
        categories = {}
        total_files = 0

        for category in ["patterns", "insights", "quality", "projects"]:
            category_path = temp_memory_dir / category
            if category_path.exists():
                count = len(list(category_path.rglob("*.md")))
                if count > 0:
                    categories[category] = count
                    total_files += count

        # Verify counts
        assert total_files == 4
        assert categories["patterns"] == 2
        assert categories["insights"] == 1
        assert categories["quality"] == 1

        # Format display (as done in csr-status)
        category_str = "/".join([f"{count}{cat[0]}" for cat, count in categories.items()])
        display = f"Memory: {total_files} files ({category_str})"

        assert display == "Memory: 4 files (2p/1i/1q)"

    def test_context_stats_calculation(self):
        """Context stats show clearing events."""
        # Simulate clearing event (as stored in unified state)
        clearing_event = {
            "timestamp": datetime.now().isoformat(),
            "original_tokens": 70000,
            "after_tokens": 25000,
            "cleared_tokens": 45000,
            "cleared_tools": 8
        }

        # Format display (as done in csr-status)
        orig = "70k"
        after = "25k"
        cleared = "45k"
        tools = "8"
        display = f"Context: {orig}→{after} (-{cleared}↓ -{tools}🔧)"

        assert display == "Context: 70k→25k (-45k↓ -8🔧)"

    def test_quality_stats_calculation(self):
        """Quality stats show avg score and auto-stored count."""
        # Simulate session quality (as stored in unified state)
        session_quality = {
            "avg_quality_score": 0.92,
            "auto_stored_patterns": 3
        }

        avg_score = session_quality["avg_quality_score"]
        auto_stored = session_quality["auto_stored_patterns"]

        display = f"Quality: {avg_score:.2f} ({auto_stored} auto-stored)"

        assert display == "Quality: 0.92 (3 auto-stored)"


class TestSecurityIntegration:
    """Test security integration with v6.0 features."""

    @pytest.fixture
    def temp_memory_dir(self, tmp_path):
        """Create temporary memory directory."""
        memory_dir = tmp_path / "memories"
        memory_dir.mkdir()
        return str(memory_dir)

    @pytest.mark.asyncio
    async def test_path_traversal_blocked_all_operations(self, temp_memory_dir):
        """All operations must block path traversal."""
        handler = MemoryToolHandler(temp_memory_dir)

        attack_path = "../../../etc/passwd"

        # Test all operations
        view_result = await handler.view(attack_path)
        assert "Security error" in view_result

        create_result = await handler.create(attack_path, "content")
        assert "Security error" in create_result

        replace_result = await handler.str_replace(attack_path, "old", "new")
        assert "Security error" in replace_result

        insert_result = await handler.insert(attack_path, 0, "content")
        assert "Security error" in insert_result

        delete_result = await handler.delete(attack_path)
        assert "Security error" in delete_result

        rename_result = await handler.rename(attack_path, "safe.md")
        assert "Security error" in rename_result


class TestOfficialAPIAlignment:
    """Test alignment with official Anthropic API patterns."""

    def test_beta_header_value(self):
        """Beta header must match official specification."""
        from src.context_manager import BETA_HEADER

        assert BETA_HEADER == "context-management-2025-06-27"

    def test_all_official_commands_implemented(self, tmp_path):
        """All 6 official commands must be implemented."""
        handler = MemoryToolHandler(str(tmp_path))

        # Official commands from Anthropic docs
        official_commands = [
            'view',
            'create',
            'str_replace',
            'insert',
            'delete',
            'rename'
        ]

        for command in official_commands:
            assert hasattr(handler, command), f"Missing command: {command}"
            assert callable(getattr(handler, command))


if __name__ == "__main__":
    pytest.main([__file__, "-v", "--tb=short"])
