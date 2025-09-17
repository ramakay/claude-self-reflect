#!/usr/bin/env python3
"""
Simple health check test for MCP server.
This test verifies the server can start and basic functionality works.
"""

import sys
import os
import asyncio
import subprocess
import time

# Add parent directory to path for imports
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))


def test_server_imports():
    """Test that all critical server imports work."""
    try:
        import src.server
        import src.search_tools
        import src.temporal_tools
        import src.reflection_tools
        assert True, "All imports successful"
    except ImportError as e:
        assert False, f"Import failed: {e}"


def test_server_startup():
    """Test that the MCP server can start without errors."""
    # Test running the server module
    result = subprocess.run(
        ["python", "-m", "src", "--help"],
        cwd=os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
        capture_output=True,
        text=True,
        timeout=5
    )

    # Check for common error patterns
    assert "Traceback" not in result.stderr, f"Server has traceback: {result.stderr}"
    assert "ModuleNotFoundError" not in result.stderr, f"Missing module: {result.stderr}"
    assert "ImportError" not in result.stderr, f"Import error: {result.stderr}"
    assert result.returncode == 0 or "--help" in result.stdout, f"Server failed to start: {result.stderr}"


def test_dependencies_installed():
    """Test that all required dependencies are installed."""
    required_modules = [
        "aiofiles",  # Added by quality-fixer
        "fastmcp",
        "qdrant_client",
        "fastembed",
        "pydantic"
    ]

    for module_name in required_modules:
        try:
            __import__(module_name.replace("-", "_"))
        except ImportError:
            assert False, f"Required module '{module_name}' is not installed"

    assert True, "All dependencies installed"


async def test_async_functions_exist():
    """Test that key async functions are defined."""
    from src.server import mcp

    # Check that tools are registered using internal method
    tools = await mcp._list_tools()
    tool_names = [tool.name for tool in tools]

    # Essential tools that must exist
    essential_tools = [
        "reflect_on_past",
        "store_reflection",
        "get_recent_work",
        "search_by_recency"
    ]

    for tool_name in essential_tools:
        assert tool_name in tool_names, f"Essential tool '{tool_name}' not found"

    assert len(tool_names) >= 10, f"Expected at least 10 tools, found {len(tool_names)}"


def test_regression_after_quality_fix():
    """
    Regression test to ensure quality-fixer changes don't break the server.
    This test specifically checks for issues introduced by automated fixes.
    """
    # Check that aiofiles is available (added by quality-fixer)
    try:
        import aiofiles
        assert True, "aiofiles import successful"
    except ImportError:
        assert False, "aiofiles not installed - quality-fixer regression"

    # Check that state classes exist (refactored by quality-fixer)
    from src.server import EmbeddingState, IndexingState

    # Verify they can be instantiated
    embedding_state = EmbeddingState()
    indexing_state = IndexingState()

    assert embedding_state is not None, "EmbeddingState instantiation failed"
    assert indexing_state is not None, "IndexingState instantiation failed"


if __name__ == "__main__":
    print("Running MCP Server Health Tests...")

    # Run synchronous tests
    test_server_imports()
    print("✅ Server imports successful")

    test_dependencies_installed()
    print("✅ All dependencies installed")

    test_server_startup()
    print("✅ Server startup successful")

    test_regression_after_quality_fix()
    print("✅ No regression from quality fixes")

    # Run async test
    asyncio.run(test_async_functions_exist())
    print("✅ Async functions verified")

    print("\n✨ All MCP server health tests passed!")