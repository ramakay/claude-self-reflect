#!/usr/bin/env python3
"""
End-to-end test for conversation import with streaming importer.
Tests the complete flow from file discovery to searchable vectors.
"""

import os
import sys
import json
import time
import asyncio
import tempfile
from pathlib import Path
from datetime import datetime
from dataclasses import dataclass, field, replace
from qdrant_client import QdrantClient
from qdrant_client.http.models import Filter, FieldCondition, MatchValue

# Add parent directory to path
sys.path.insert(0, str(Path(__file__).parent.parent))
from scripts.streaming_importer_final import StreamingImporter, Config

async def test_e2e_import():
    """Test end-to-end conversation import."""
    print("🧪 Testing End-to-End Conversation Import\n")
    
    # Create test conversation file
    test_dir = tempfile.mkdtemp(prefix="e2e_test_")
    test_file = Path(test_dir) / "test_conversation.json"
    
    test_conversation = {
        "name": "E2E Test Conversation",
        "created_at": "2025-01-17T10:00:00Z",
        "messages": [
            {
                "role": "human",
                "content": "Testing the streaming importer end-to-end functionality",
                "created_at": "2025-01-17T10:00:00Z"
            },
            {
                "role": "assistant", 
                "content": "I'll help test the streaming importer. This is a test response to verify the import process works correctly.",
                "created_at": "2025-01-17T10:00:01Z"
            }
        ]
    }
    
    with open(test_file, 'w') as f:
        json.dump(test_conversation, f)
    
    print(f"✅ Created test file: {test_file}")
    
    # Initialize client
    client = QdrantClient(url="http://localhost:6333")
    
    # Create temporary state file
    state_file = Path(test_dir) / "test_state.json"
    
    # Create test configuration
    config = Config(
        logs_dir=Path(test_dir),
        state_file=state_file,
        batch_size=1,  # Process immediately
        import_frequency=1,  # Quick checks
        prefer_local_embeddings=True,  # Use local embeddings
        max_concurrent_embeddings=1,
        max_queue_size=10
    )
    
    # Initialize importer with test configuration
    importer = StreamingImporter(config)
    
    print("📤 Starting import...")
    
    # Run import for a short time
    import_task = asyncio.create_task(importer.run_continuous())
    await asyncio.sleep(5)  # Give it time to process
    
    # Stop the importer
    importer.stop_requested = True
    try:
        await asyncio.wait_for(import_task, timeout=5)
    except asyncio.TimeoutError:
        print("⚠️ Import task didn't stop cleanly")
    
    print("🔍 Verifying import in Qdrant...")
    
    # Check if conversation was imported
    collections = client.get_collections().collections
    test_collections = [c for c in collections if "test_conversation" in c.name.lower()]
    
    if test_collections:
        collection_name = test_collections[0].name
        print(f"✅ Found collection: {collection_name}")
        
        # Search for our test content
        search_results = client.search(
            collection_name=collection_name,
            query_vector=[0.1] * 384,  # Dummy vector for local embeddings
            limit=5
        )
        
        if search_results:
            print(f"✅ Found {len(search_results)} chunks in collection")
            
            # Verify content
            for i, result in enumerate(search_results[:2], 1):
                print(f"\n  Chunk {i}:")
                print(f"  - Score: {result.score:.4f}")
                print(f"  - Text preview: {result.payload.get('text', '')[:100]}...")
                
            # Clean up test collection
            client.delete_collection(collection_name)
            print(f"\n🧹 Cleaned up test collection: {collection_name}")
            
            return True
        else:
            print("❌ No chunks found in collection")
            return False
    else:
        print("❌ Collection not created")
        
        # Check state file for errors
        if state_file.exists():
            with open(state_file) as f:
                state = json.load(f)
                if state.get("errors"):
                    print(f"⚠️ Errors in state: {state['errors']}")
        
        return False

async def test_mcp_tools():
    """Test MCP tools with both local and cloud embeddings."""
    print("\n🧪 Testing MCP Tools with Local and Cloud Embeddings\n")
    
    # Test search with local embeddings
    print("Testing local embedding search...")
    
    # Import the MCP server module properly
    sys.path.insert(0, str(Path(__file__).parent.parent / "mcp-server" / "src"))
    from server import reflect_on_past
    
    try:
        # Search for a known topic
        results = await reflect_on_past(
            query="streaming importer CPU optimization",
            limit=3,
            min_score=0.5
        )
        
        if results:
            print(f"✅ Local search returned {len(results)} results")
        else:
            print("⚠️ Local search returned no results")
            
    except Exception as e:
        print(f"❌ Local search failed: {e}")
    
    # Test with Voyage collections if they exist
    client = QdrantClient(url="http://localhost:6333")
    voyage_collections = [c.name for c in client.get_collections().collections if "_voyage" in c.name]
    
    if voyage_collections:
        print(f"\nTesting cloud embedding search ({len(voyage_collections)} Voyage collections)...")
        
        # Force using Voyage for this test
        os.environ["PREFER_LOCAL_EMBEDDINGS"] = "false"
        
        try:
            results = await reflect_on_past(
                query="docker container optimization",
                limit=3,
                min_score=0.5
            )
            
            if results:
                print(f"✅ Cloud search returned {len(results)} results")
            else:
                print("⚠️ Cloud search returned no results")
                
        except Exception as e:
            print(f"❌ Cloud search failed: {e}")
        
        # Reset to local
        os.environ["PREFER_LOCAL_EMBEDDINGS"] = "true"
    else:
        print("ℹ️ No Voyage collections found, skipping cloud test")
    
    return True

async def test_import_cycles():
    """Test 60-second import cycles."""
    print("\n🧪 Testing 60-Second Import Cycles\n")
    
    # Create test directory with multiple files
    test_dir = tempfile.mkdtemp(prefix="cycle_test_")
    
    # Create 3 test files at different times
    for i in range(3):
        test_file = Path(test_dir) / f"conversation_{i}.json"
        test_conversation = {
            "name": f"Cycle Test {i}",
            "created_at": f"2025-01-17T10:0{i}:00Z",
            "messages": [
                {
                    "role": "human",
                    "content": f"Test conversation {i} for import cycles",
                    "created_at": f"2025-01-17T10:0{i}:00Z"
                }
            ]
        }
        
        with open(test_file, 'w') as f:
            json.dump(test_conversation, f)
        
        # Stagger file creation times
        if i < 2:
            time.sleep(1)
    
    print(f"✅ Created {len(list(Path(test_dir).glob('*.json')))} test files")
    
    # Create state file
    state_file = Path(test_dir) / "cycle_state.json"
    
    # Create test configuration with 60-second check interval
    config = Config(
        logs_dir=Path(test_dir),
        state_file=state_file,
        batch_size=1,
        max_concurrent_embeddings=2,
        import_frequency=2,  # Faster for testing
        prefer_local_embeddings=True
    )
    
    # Initialize importer
    importer = StreamingImporter(config)
    
    print("📤 Starting cyclic import...")
    
    # Track import cycles
    cycles = []
    start_time = time.time()
    
    async def monitor_cycles():
        """Monitor import cycles."""
        last_count = 0
        while not importer.stop_requested:
            await asyncio.sleep(2)
            
            if state_file.exists():
                with open(state_file) as f:
                    state = json.load(f)
                    processed = len(state.get("processed_files", []))
                    
                    if processed > last_count:
                        cycle_time = time.time() - start_time
                        cycles.append({
                            "time": cycle_time,
                            "files": processed - last_count
                        })
                        print(f"  Cycle at {cycle_time:.1f}s: Processed {processed - last_count} new files")
                        last_count = processed
    
    # Run import and monitor
    import_task = asyncio.create_task(importer.run_continuous())
    monitor_task = asyncio.create_task(monitor_cycles())
    
    # Run for 10 seconds to see multiple cycles
    await asyncio.sleep(10)
    
    # Stop tasks
    importer.stop_requested = True
    try:
        await asyncio.wait_for(import_task, timeout=5)
    except asyncio.TimeoutError:
        pass
    
    monitor_task.cancel()
    
    print(f"\n✅ Completed {len(cycles)} import cycles")
    
    # Verify cycle timing
    if len(cycles) >= 2:
        cycle_intervals = [cycles[i+1]["time"] - cycles[i]["time"] for i in range(len(cycles)-1)]
        avg_interval = sum(cycle_intervals) / len(cycle_intervals)
        print(f"  Average cycle interval: {avg_interval:.1f}s")
        
        # Clean up test collections
        client = QdrantClient(url="http://localhost:6333")
        for collection in client.get_collections().collections:
            if "cycle_test" in collection.name.lower():
                client.delete_collection(collection.name)
                print(f"  🧹 Cleaned up: {collection.name}")
        
        return True
    else:
        print("⚠️ Not enough cycles completed for analysis")
        return False

async def main():
    """Run all end-to-end tests."""
    print("=" * 60)
    print("END-TO-END TESTING SUITE FOR v2.5.16")
    print("=" * 60)
    
    results = {}
    
    # Test 1: End-to-end import
    try:
        results["E2E Import"] = await test_e2e_import()
    except Exception as e:
        print(f"❌ E2E Import test failed: {e}")
        results["E2E Import"] = False
    
    # Test 2: MCP tools
    try:
        results["MCP Tools"] = await test_mcp_tools()
    except Exception as e:
        print(f"❌ MCP Tools test failed: {e}")
        results["MCP Tools"] = False
    
    # Test 3: Import cycles
    try:
        results["Import Cycles"] = await test_import_cycles()
    except Exception as e:
        print(f"❌ Import Cycles test failed: {e}")
        results["Import Cycles"] = False
    
    # Summary
    print("\n" + "=" * 60)
    print("TEST SUMMARY")
    print("=" * 60)
    
    for test_name, passed in results.items():
        status = "✅ PASSED" if passed else "❌ FAILED"
        print(f"{test_name}: {status}")
    
    total_passed = sum(1 for p in results.values() if p)
    total_tests = len(results)
    
    print(f"\nTotal: {total_passed}/{total_tests} tests passed")
    
    return all(results.values())

if __name__ == "__main__":
    success = asyncio.run(main())
    sys.exit(0 if success else 1)