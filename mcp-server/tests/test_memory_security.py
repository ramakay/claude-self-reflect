"""
Security Tests for Memory Tool - Path Traversal Prevention

Critical test suite validating path traversal protection in memory_tools.py
following recommendations from quality-analysis-2025-10-01.md
"""

import sys
import os
import pytest
import tempfile
from pathlib import Path

# Add parent directory to path for imports
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from src.memory_tools import MemoryToolHandler


class TestPathTraversalPrevention:
    """Critical security tests for path traversal attacks."""

    @pytest.fixture
    def temp_memory_dir(self):
        """Create isolated temporary directory for testing."""
        with tempfile.TemporaryDirectory() as tmpdir:
            yield tmpdir

    @pytest.fixture
    def handler(self, temp_memory_dir):
        """Create handler with isolated base path."""
        return MemoryToolHandler(base_path=temp_memory_dir)

    # ==================== Path Traversal Attack Tests ====================

    @pytest.mark.asyncio
    async def test_prevents_parent_directory_traversal(self, handler):
        """CRITICAL: Block ../../../etc/passwd style attacks."""
        result = await handler.view("../../../etc/passwd")
        assert "Security error" in result
        assert "Path traversal detected" in result

    @pytest.mark.asyncio
    async def test_prevents_absolute_path_access(self, handler):
        """CRITICAL: Block /etc/passwd style absolute paths."""
        result = await handler.view("/etc/passwd")
        # Either security error or file not found (after stripping /) is acceptable
        assert ("Security error" in result or "Memory file not found" in result)

    @pytest.mark.asyncio
    async def test_prevents_relative_escape_with_subdirs(self, handler):
        """CRITICAL: Block memories/../../secrets style escapes."""
        result = await handler.view("patterns/../../secrets/private.key")
        assert "Security error" in result
        assert "Path traversal detected" in result

    @pytest.mark.asyncio
    async def test_prevents_symlink_escape(self, handler, temp_memory_dir):
        """CRITICAL: Block symlink-based escapes."""
        # Create a symlink pointing outside base directory
        symlink_path = Path(temp_memory_dir) / "patterns" / "escape_link"
        symlink_path.parent.mkdir(parents=True, exist_ok=True)

        # Try to create symlink to /etc (should be blocked when accessed)
        try:
            symlink_path.symlink_to("/etc")
        except OSError:
            pytest.skip("Cannot create symlink (permissions)")

        # Accessing through symlink should fail validation
        result = await handler.view("patterns/escape_link/passwd")
        assert "Security error" in result
        assert "Path traversal detected" in result

    @pytest.mark.asyncio
    async def test_prevents_windows_drive_access(self, handler):
        """CRITICAL: Block C:\\ or \\\\server\\ style paths."""
        attack_paths = [
            "C:\\Windows\\System32",
            "\\\\server\\share\\secrets",
            "D:\\secrets\\data.txt"
        ]

        for attack_path in attack_paths:
            result = await handler.view(attack_path)
            # Either security error or file not found is acceptable (prevented access)
            assert ("Security error" in result or "Memory file not found" in result)

    # ==================== Valid Path Tests ====================

    @pytest.mark.asyncio
    async def test_allows_valid_patterns_path(self, handler):
        """Valid paths within base directory should work."""
        result = await handler.create("patterns/test_code.md", "# Test Pattern\n\nGood code example")
        assert "✅" in result
        assert "patterns/test_code.md" in result

    @pytest.mark.asyncio
    async def test_allows_valid_subdirectory_path(self, handler):
        """Nested valid paths should work."""
        result = await handler.create(
            "quality/2025/analysis.md",
            "# Quality Analysis\n\nMetrics data"
        )
        assert "✅" in result

    @pytest.mark.asyncio
    async def test_allows_path_with_dots_in_name(self, handler):
        """Paths with dots in filename (not traversal) should work."""
        result = await handler.create(
            "insights/api.v2.notes.md",
            "# API v2 Notes"
        )
        assert "✅" in result

    # ==================== CRUD Security Tests ====================

    @pytest.mark.asyncio
    async def test_create_prevents_traversal(self, handler):
        """Create operation must validate paths."""
        result = await handler.create("../../../tmp/malicious.md", "evil content")
        assert "Security error" in result
        assert "Path traversal detected" in result

    @pytest.mark.asyncio
    async def test_str_replace_prevents_traversal(self, handler):
        """Update operation must validate paths."""
        result = await handler.str_replace(
            "../../../tmp/target.md",
            "old",
            "new"
        )
        assert "Security error" in result
        assert "Path traversal detected" in result

    @pytest.mark.asyncio
    async def test_delete_prevents_traversal(self, handler):
        """Delete operation must validate paths."""
        result = await handler.delete("../../../tmp/victim.md")
        assert "Security error" in result
        assert "Path traversal detected" in result

    @pytest.mark.asyncio
    async def test_rename_prevents_source_traversal(self, handler):
        """Rename source path must be validated."""
        # Create valid destination first
        await handler.create("patterns/valid.md", "content")

        result = await handler.rename(
            "../../../tmp/malicious.md",
            "patterns/valid.md"
        )
        assert "Security error" in result
        assert "Path traversal detected" in result

    @pytest.mark.asyncio
    async def test_rename_prevents_destination_traversal(self, handler):
        """Rename destination path must be validated."""
        # Create valid source first
        await handler.create("patterns/source.md", "content")

        result = await handler.rename(
            "patterns/source.md",
            "../../../tmp/escaped.md"
        )
        assert "Security error" in result
        assert "Path traversal detected" in result

    # ==================== Edge Cases ====================

    @pytest.mark.asyncio
    async def test_handles_url_encoded_traversal(self, handler):
        """Block URL-encoded path traversal attempts."""
        encoded_attacks = [
            "%2e%2e%2f%2e%2e%2f%2e%2e%2fetc%2fpasswd",  # ../../../etc/passwd
            "..%2F..%2F..%2Fetc%2Fpasswd",
            "%2e%2e/%2e%2e/%2e%2e/etc/passwd"
        ]

        for attack in encoded_attacks:
            result = await handler.view(attack)
            # Either security error or file not found is acceptable (prevented access)
            assert ("Security error" in result or "Memory file not found" in result)

    @pytest.mark.asyncio
    async def test_handles_null_byte_injection(self, handler):
        """Block null byte injection attempts."""
        # Null byte could terminate string early in some systems
        result = await handler.view("../../../etc/passwd\x00.md")
        assert "Security error" in result
        # Either path traversal or null byte error is acceptable
        assert ("Path traversal detected" in result or "null character" in result)

    @pytest.mark.asyncio
    async def test_handles_unicode_traversal(self, handler):
        """Block Unicode-encoded traversal attempts."""
        # Unicode variants of ../
        unicode_attacks = [
            "\u2024\u2024/\u2024\u2024/etc/passwd",  # Unicode dots
            "..\uff0f..\uff0fetc\uff0fpasswd",  # Fullwidth solidus
        ]

        for attack in unicode_attacks:
            result = await handler.view(attack)
            # Either security error or file not found is acceptable (prevented access)
            assert ("Security error" in result or "Memory file not found" in result)

    # ==================== Logging Security Tests ====================

    @pytest.mark.asyncio
    async def test_logs_security_violations(self, handler, caplog):
        """Security violations should be logged for monitoring."""
        import logging
        caplog.set_level(logging.WARNING)

        await handler.view("../../../etc/passwd")

        # Check that security violation was logged
        assert any(
            "Security violation" in record.message or "Path traversal detected" in record.message
            for record in caplog.records
        )

    @pytest.mark.asyncio
    async def test_does_not_leak_base_path_in_error(self, handler):
        """Error messages should not expose system paths."""
        error_msg = await handler.view("../../../etc/passwd")

        # Should contain generic message, not full system paths
        assert "Path traversal detected" in error_msg
        # Should not leak actual base directory path
        assert "/tmp/" not in error_msg or "tmpdir" not in error_msg


class TestMemoryToolSecurity:
    """Additional security validations."""

    @pytest.fixture
    def handler(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            yield MemoryToolHandler(base_path=tmpdir)

    @pytest.mark.asyncio
    async def test_search_does_not_escape_base(self, handler):
        """Search should only access files within base directory."""
        # Create a file in base
        await handler.create("patterns/safe.md", "searchable content")

        # Search should only find files in base
        results = await handler.search("searchable")

        for result in results:
            # All paths should be relative to base
            assert not result['path'].startswith('/')
            assert '..' not in result['path']

    @pytest.mark.asyncio
    async def test_list_memories_does_not_escape_base(self, handler):
        """List operation should only access base directory."""
        # Create files in base
        await handler.create("patterns/file1.md", "content")
        await handler.create("insights/file2.md", "content")

        # List all
        memories = await handler.list_memories()

        for mem in memories:
            # All paths should be relative to base
            assert not mem['path'].startswith('/')
            assert '..' not in mem['path']

    @pytest.mark.asyncio
    async def test_prevents_directory_creation_outside_base(self, handler):
        """Creating files should not create directories outside base."""
        result = await handler.create(
            "../../../tmp/malicious/dir/file.md",
            "evil content"
        )
        assert "Security error" in result
        assert "Path traversal detected" in result


if __name__ == "__main__":
    pytest.main([__file__, "-v", "--tb=short"])
