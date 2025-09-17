#!/usr/bin/env python3
"""
Test MCP search functionality directly.
"""

import os
import sys
import json
import asyncio
from pathlib import Path
from qdrant_client import QdrantClient

# Add MCP server to path
sys.path.insert(0, str(Path(__file__).parent.parent / "mcp-server" / "src"))

async def test_mcp_search():
    """Test MCP search functionality."""
    print("🧪 Testing MCP Search Functionality\n")
    
    # Import the actual search functions from the server module
    from server import SearchParams, search_conversations
    
    # Test with local embeddings
    print("1. Testing with local embeddings (FastEmbed)...")
    os.environ["PREFER_LOCAL_EMBEDDINGS"] = "true"
    
    params = SearchParams(
        query="streaming importer CPU optimization",
        limit=3,
        min_score=0.5,
        use_decay=-1  # Use default
    )
    
    try:
        results = await search_conversations(params)
        
        if results and results.get("results"):
            print(f"   ✅ Found {len(results['results'])} results")
            for i, result in enumerate(results["results"][:2], 1):
                print(f"   Result {i}: Score {result.get('score', 0):.3f}")
        else:
            print("   ⚠️ No results found")
    except Exception as e:
        print(f"   ❌ Search failed: {e}")
    
    # Test with Voyage collections if they exist
    client = QdrantClient(url="http://localhost:6333")
    collections = client.get_collections().collections
    voyage_collections = [c.name for c in collections if "_voyage" in c.name]
    
    if voyage_collections:
        print(f"\n2. Testing with cloud embeddings (Voyage AI) - {len(voyage_collections)} collections...")
        os.environ["PREFER_LOCAL_EMBEDDINGS"] = "false"
        
        params = SearchParams(
            query="docker container performance",
            limit=3,
            min_score=0.5
        )
        
        try:
            results = await search_conversations(params)
            
            if results and results.get("results"):
                print(f"   ✅ Found {len(results['results'])} results")
                for i, result in enumerate(results["results"][:2], 1):
                    print(f"   Result {i}: Score {result.get('score', 0):.3f}")
            else:
                print("   ⚠️ No results found")
        except Exception as e:
            print(f"   ❌ Search failed: {e}")
    else:
        print("\n2. No Voyage collections found - skipping cloud test")
    
    # Test quick search
    print("\n3. Testing quick search...")
    from server import quick_search
    
    try:
        result = await quick_search("memory optimization")
        if result:
            print(f"   ✅ Quick search successful")
            print(f"   Total matches: {result.get('total_count', 0)}")
            if result.get("top_result"):
                print(f"   Top score: {result['top_result'].get('score', 0):.3f}")
        else:
            print("   ⚠️ No quick search results")
    except Exception as e:
        print(f"   ❌ Quick search failed: {e}")
    
    # Test search by file
    print("\n4. Testing search by file...")
    from server import search_by_file
    
    try:
        result = await search_by_file("streaming_importer_final.py", limit=5, project=None)
        if result and result.get("results"):
            print(f"   ✅ Found {len(result['results'])} conversations mentioning the file")
        else:
            print("   ⚠️ No conversations found for this file")
    except Exception as e:
        print(f"   ❌ Search by file failed: {e}")
    
    # Test search by concept
    print("\n5. Testing search by concept...")
    from server import search_by_concept
    
    try:
        result = await search_by_concept("docker", limit=5, project=None)
        if result and result.get("results"):
            print(f"   ✅ Found {len(result['results'])} conversations about docker")
        else:
            print("   ⚠️ No conversations found for this concept")
    except Exception as e:
        print(f"   ❌ Search by concept failed: {e}")
    
    return True

if __name__ == "__main__":
    success = asyncio.run(test_mcp_search())
    print("\n" + "=" * 60)
    print("MCP Search Test Complete")
    print("=" * 60)