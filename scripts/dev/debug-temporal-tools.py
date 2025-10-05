#!/usr/bin/env python3
"""
Debug script for testing temporal tools in Claude Self Reflect.
This script directly tests the temporal tools that should be available via MCP.
"""

import os
import sys
import asyncio
import json
import traceback
from pathlib import Path

# Add the mcp-server source to Python path
sys.path.append(str(Path(__file__).parent.parent / "mcp-server" / "src"))

os.environ["QDRANT_URL"] = "http://localhost:6333"

async def test_temporal_tools():
    """Test all temporal tools."""
    print("=== TEMPORAL TOOLS DEBUG SCRIPT ===")
    
    try:
        # Import required modules
        from server import (
            get_recent_work, search_by_recency, get_timeline,
            get_all_collections, QDRANT_URL
        )
        from fastmcp import Context
        
        print(f"✅ Successfully imported temporal tools")
        print(f"✅ Qdrant URL: {QDRANT_URL}")
        
        # Check if Qdrant is available
        collections = await get_all_collections()
        print(f"✅ Found {len(collections)} collections: {collections[:5]}...")
        
        # Create a mock context for testing
        class MockContext(Context):
            def __init__(self):
                pass
            async def debug(self, message):
                print(f"DEBUG: {message}")
            async def error(self, message):
                print(f"ERROR: {message}")
        
        ctx = MockContext()
        
        # Test 1: get_recent_work with default parameters
        print("\n--- Test 1: get_recent_work (default) ---")
        try:
            result = await get_recent_work(ctx)
            print(f"✅ get_recent_work succeeded")
            print(f"Result length: {len(result) if result else 0} characters")
            if result and len(result) < 500:
                print(f"Result: {result}")
        except Exception as e:
            print(f"❌ get_recent_work failed: {e}")
            traceback.print_exc()
        
        # Test 2: get_recent_work with project='all'
        print("\n--- Test 2: get_recent_work (project=all) ---")
        try:
            result = await get_recent_work(ctx, project="all", limit=5)
            print(f"✅ get_recent_work (project=all) succeeded")
            print(f"Result length: {len(result) if result else 0} characters")
        except Exception as e:
            print(f"❌ get_recent_work (project=all) failed: {e}")
            traceback.print_exc()
        
        # Test 3: get_recent_work with different group_by options
        for group_by in ["conversation", "day", "session"]:
            print(f"\n--- Test 3.{group_by}: get_recent_work (group_by={group_by}) ---")
            try:
                result = await get_recent_work(ctx, limit=3, group_by=group_by)
                print(f"✅ get_recent_work (group_by={group_by}) succeeded")
                print(f"Result length: {len(result) if result else 0} characters")
            except Exception as e:
                print(f"❌ get_recent_work (group_by={group_by}) failed: {e}")
                traceback.print_exc()
        
        # Test 4: search_by_recency with time_range
        print("\n--- Test 4: search_by_recency (time_range) ---")
        try:
            result = await search_by_recency(
                ctx,
                query="testing debugging",
                time_range="last week",
                limit=5
            )
            print(f"✅ search_by_recency (time_range) succeeded")
            print(f"Result length: {len(result) if result else 0} characters")
        except Exception as e:
            print(f"❌ search_by_recency (time_range) failed: {e}")
            traceback.print_exc()
        
        # Test 5: search_by_recency with since/until
        print("\n--- Test 5: search_by_recency (since/until) ---")
        try:
            result = await search_by_recency(
                ctx,
                query="python script",
                since="yesterday",
                limit=3
            )
            print(f"✅ search_by_recency (since/until) succeeded")  
            print(f"Result length: {len(result) if result else 0} characters")
        except Exception as e:
            print(f"❌ search_by_recency (since/until) failed: {e}")
            traceback.print_exc()
        
        # Test 6: get_timeline with different granularities
        for granularity in ["day", "week"]:
            print(f"\n--- Test 6.{granularity}: get_timeline (granularity={granularity}) ---")
            try:
                result = await get_timeline(
                    ctx,
                    time_range="last week", 
                    granularity=granularity,
                    include_stats=True
                )
                print(f"✅ get_timeline (granularity={granularity}) succeeded")
                print(f"Result length: {len(result) if result else 0} characters")
            except Exception as e:
                print(f"❌ get_timeline (granularity={granularity}) failed: {e}")
                traceback.print_exc()
        
        print("\n=== TEMPORAL TOOLS TEST COMPLETE ===")
        
    except Exception as e:
        print(f"❌ Critical error during setup: {e}")
        traceback.print_exc()

if __name__ == "__main__":
    asyncio.run(test_temporal_tools())
