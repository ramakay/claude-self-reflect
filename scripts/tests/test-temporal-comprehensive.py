#!/usr/bin/env python3
"""Comprehensive test for temporal query functionality."""

import asyncio
import json
import sys
import os
from datetime import datetime, timezone, timedelta
from pathlib import Path
from typing import List, Dict

# Add parent directory to path for imports
sys.path.insert(0, str(Path(__file__).parent.parent))

from qdrant_client import AsyncQdrantClient
from qdrant_client.models import PointStruct, VectorParams, Distance
from fastembed import TextEmbedding
import uuid

# Configuration
QDRANT_URL = os.getenv("QDRANT_URL", "http://localhost:6333")
TEST_COLLECTION = "test_temporal_local"

async def setup_test_data():
    """Create test collection with temporal conversation data."""
    print("Setting up test data...")
    
    # Initialize clients
    client = AsyncQdrantClient(url=QDRANT_URL)
    embedding_model = TextEmbedding(model_name="sentence-transformers/all-MiniLM-L6-v2")
    
    # Delete collection if exists
    try:
        await client.delete_collection(TEST_COLLECTION)
        print(f"Deleted existing collection {TEST_COLLECTION}")
    except:
        pass
    
    # Create collection
    await client.create_collection(
        collection_name=TEST_COLLECTION,
        vectors_config=VectorParams(size=384, distance=Distance.COSINE)
    )
    print(f"Created collection {TEST_COLLECTION}")
    
    # Create index on timestamp field for OrderBy support
    from qdrant_client.models import PayloadSchemaType
    await client.create_payload_index(
        collection_name=TEST_COLLECTION,
        field_name="timestamp",
        field_schema=PayloadSchemaType.DATETIME
    )
    print("Created timestamp index for OrderBy support")
    
    # Generate test conversations with different timestamps
    now = datetime.now(timezone.utc)
    test_data = [
        # Today's conversations
        {
            "text": "Working on temporal query implementation for Claude Self Reflect",
            "timestamp": now.isoformat(),
            "project": "claude-self-reflect",
            "conversation_id": str(uuid.uuid4()),
            "files_analyzed": ["server.py", "temporal_utils.py"],
            "tools_used": ["mcp", "qdrant"],
            "concepts": ["temporal queries", "MCP tools"]
        },
        {
            "text": "Fixed embedding generation bug in search_by_recency function",
            "timestamp": (now - timedelta(hours=2)).isoformat(),
            "project": "claude-self-reflect",
            "conversation_id": str(uuid.uuid4()),
            "files_edited": ["server.py"],
            "tools_used": ["python", "debugging"],
            "concepts": ["bug fix", "embeddings"]
        },
        
        # Yesterday's conversations
        {
            "text": "Reviewed Qdrant documentation for native temporal features",
            "timestamp": (now - timedelta(days=1, hours=3)).isoformat(),
            "project": "claude-self-reflect",
            "conversation_id": str(uuid.uuid4()),
            "tools_used": ["context7", "qdrant"],
            "concepts": ["documentation", "research"]
        },
        
        # Last week's conversations
        {
            "text": "Implemented session detection algorithm for grouping work",
            "timestamp": (now - timedelta(days=5)).isoformat(),
            "project": "claude-self-reflect",
            "conversation_id": str(uuid.uuid4()),
            "files_edited": ["temporal_utils.py"],
            "concepts": ["session detection", "algorithms"]
        },
        {
            "text": "Designed API for temporal query tools",
            "timestamp": (now - timedelta(days=6)).isoformat(),
            "project": "claude-self-reflect",
            "conversation_id": str(uuid.uuid4()),
            "files_analyzed": ["temporal_design.py"],
            "concepts": ["API design", "planning"]
        },
        
        # Last month's conversations
        {
            "text": "Initial planning for memory decay feature",
            "timestamp": (now - timedelta(days=25)).isoformat(),
            "project": "claude-self-reflect",
            "conversation_id": str(uuid.uuid4()),
            "concepts": ["memory decay", "planning"]
        },
        
        # Old conversation (3 months ago)
        {
            "text": "Setup initial Qdrant integration",
            "timestamp": (now - timedelta(days=90)).isoformat(),
            "project": "claude-self-reflect",
            "conversation_id": str(uuid.uuid4()),
            "concepts": ["setup", "integration"]
        }
    ]
    
    # Generate embeddings and create points
    points = []
    for i, data in enumerate(test_data):
        # Generate embedding
        embeddings = list(embedding_model.embed([data["text"]]))
        embedding = embeddings[0]
        
        # Create point
        point = PointStruct(
            id=str(uuid.uuid4()),
            vector=embedding.tolist() if hasattr(embedding, 'tolist') else list(embedding),
            payload=data
        )
        points.append(point)
    
    # Upsert points
    await client.upsert(
        collection_name=TEST_COLLECTION,
        points=points
    )
    print(f"Inserted {len(points)} test conversations")
    
    return len(points)

async def test_temporal_queries():
    """Test temporal query functionality."""
    print("\n" + "="*60)
    print("Testing Temporal Queries")
    print("="*60)
    
    client = AsyncQdrantClient(url=QDRANT_URL)
    
    # Test 1: OrderBy timestamp (recent work)
    print("\n1. Testing OrderBy timestamp (get recent work)...")
    from qdrant_client.models import OrderBy
    
    results, _ = await client.scroll(
        collection_name=TEST_COLLECTION,
        order_by=OrderBy(key="timestamp", direction="desc"),
        limit=3,
        with_payload=True
    )
    
    print(f"Found {len(results)} recent conversations:")
    for r in results:
        ts = r.payload.get('timestamp', 'N/A')
        text = r.payload.get('text', '')[:60]
        print(f"  - {ts}: {text}...")
    
    # Test 2: DatetimeRange filter (last week)
    print("\n2. Testing DatetimeRange filter (last week)...")
    from qdrant_client.models import Filter, FieldCondition, DatetimeRange
    
    now = datetime.now(timezone.utc)
    week_ago = now - timedelta(days=7)
    
    time_filter = Filter(
        must=[
            FieldCondition(
                key="timestamp",
                range=DatetimeRange(
                    gte=week_ago.isoformat(),
                    lte=now.isoformat()
                )
            )
        ]
    )
    
    # Generate a simple embedding for search
    embedding_model = TextEmbedding(model_name="sentence-transformers/all-MiniLM-L6-v2")
    embeddings = list(embedding_model.embed(["temporal query"]))
    query_embedding = embeddings[0].tolist() if hasattr(embeddings[0], 'tolist') else list(embeddings[0])
    
    results = await client.search(
        collection_name=TEST_COLLECTION,
        query_vector=query_embedding,
        query_filter=time_filter,
        limit=10,
        with_payload=True
    )
    
    print(f"Found {len(results)} conversations from last week:")
    for r in results:
        ts = r.payload.get('timestamp', 'N/A')
        text = r.payload.get('text', '')[:60]
        score = r.score
        print(f"  - Score {score:.3f} | {ts}: {text}...")
    
    # Test 3: Combined OrderBy + Filter (yesterday's work)
    print("\n3. Testing combined OrderBy + Filter (yesterday)...")
    
    yesterday_start = (now - timedelta(days=2)).replace(hour=0, minute=0, second=0, microsecond=0)
    yesterday_end = yesterday_start + timedelta(days=1)
    
    yesterday_filter = Filter(
        must=[
            FieldCondition(
                key="timestamp",
                range=DatetimeRange(
                    gte=yesterday_start.isoformat(),
                    lt=yesterday_end.isoformat()
                )
            )
        ]
    )
    
    results, _ = await client.scroll(
        collection_name=TEST_COLLECTION,
        scroll_filter=yesterday_filter,
        order_by=OrderBy(key="timestamp", direction="desc"),
        limit=10,
        with_payload=True
    )
    
    print(f"Found {len(results)} conversations from yesterday:")
    for r in results:
        ts = r.payload.get('timestamp', 'N/A')
        text = r.payload.get('text', '')[:60]
        print(f"  - {ts}: {text}...")
    
    # Test 4: Session detection
    print("\n4. Testing session detection...")
    sys.path.insert(0, str(Path(__file__).parent.parent / "mcp-server" / "src"))
    from temporal_utils import SessionDetector
    
    # Get all recent conversations
    all_results, _ = await client.scroll(
        collection_name=TEST_COLLECTION,
        order_by=OrderBy(key="timestamp", direction="desc"),
        limit=100,
        with_payload=True
    )
    
    chunks = []
    for r in all_results:
        chunks.append({
            'timestamp': r.payload.get('timestamp'),
            'text': r.payload.get('text', ''),
            'project': r.payload.get('project', '')
        })
    
    detector = SessionDetector(time_gap_minutes=120)  # 2 hour gap
    sessions = detector.detect_sessions(chunks)
    
    print(f"Detected {len(sessions)} work sessions:")
    for i, session in enumerate(sessions[:3]):  # Show first 3
        print(f"\n  Session {i+1}:")
        print(f"    Start: {session.start_time}")
        print(f"    End: {session.end_time}")
        print(f"    Duration: {session.duration_minutes} minutes")
        print(f"    Messages: {session.message_count}")
        if session.main_topics:
            print(f"    Topics: {', '.join(session.main_topics)}")
    
    # Test 5: Natural language time parsing
    print("\n5. Testing natural language time parsing...")
    from temporal_utils import TemporalParser
    
    parser = TemporalParser()
    test_expressions = [
        "yesterday",
        "last week",
        "this month",
        "past 3 days",
        "since monday"
    ]
    
    for expr in test_expressions:
        try:
            start, end = parser.parse_time_expression(expr)
            print(f"  '{expr}' -> from {start.isoformat()} to {end.isoformat()}")
        except Exception as e:
            print(f"  '{expr}' -> Error: {e}")
    
    print("\n" + "="*60)
    print("Temporal Query Tests Complete!")
    print("="*60)

async def test_mcp_tools():
    """Test the MCP tools directly if possible."""
    print("\n" + "="*60)
    print("Testing MCP Tools via Python")
    print("="*60)
    
    # Import the server module
    sys.path.insert(0, str(Path(__file__).parent.parent / "mcp-server" / "src"))
    
    try:
        from server import get_recent_work, search_by_recency, get_timeline
        from fastmcp import Context
        
        # Create a mock context
        class MockContext:
            async def debug(self, msg):
                print(f"[DEBUG] {msg}")
            
            async def report_progress(self, progress, total, message=""):
                pass
        
        ctx = MockContext()
        
        # Test get_recent_work
        print("\n1. Testing get_recent_work...")
        result = await get_recent_work(ctx, limit=5, project="claude-self-reflect")
        print(f"Result: {result[:500]}..." if len(result) > 500 else f"Result: {result}")
        
        # Test search_by_recency
        print("\n2. Testing search_by_recency...")
        result = await search_by_recency(ctx, query="temporal", time_range="last week")
        print(f"Result: {result[:500]}..." if len(result) > 500 else f"Result: {result}")
        
        # Test get_timeline
        print("\n3. Testing get_timeline...")
        result = await get_timeline(ctx, time_range="last 7 days", granularity="day")
        print(f"Result: {result[:500]}..." if len(result) > 500 else f"Result: {result}")
        
    except Exception as e:
        print(f"Could not test MCP tools directly: {e}")
        print("The tools need to be tested via the MCP interface in Claude")

async def main():
    """Run all tests."""
    print("Claude Self Reflect - Comprehensive Temporal Query Test")
    print("="*60)
    
    # Setup test data
    count = await setup_test_data()
    print(f"\n✅ Test setup complete: {count} test conversations created")
    
    # Run temporal query tests
    await test_temporal_queries()
    
    # Try to test MCP tools
    await test_mcp_tools()
    
    print("\n" + "="*60)
    print("All tests complete!")
    print("="*60)
    print("\nNext steps:")
    print("1. Restart the MCP server to apply code fixes")
    print("2. Test the MCP tools in Claude:")
    print("   - mcp__claude-self-reflect__get_recent_work")
    print("   - mcp__claude-self-reflect__search_by_recency")
    print("   - mcp__claude-self-reflect__get_timeline")

if __name__ == "__main__":
    asyncio.run(main())