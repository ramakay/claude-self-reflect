#!/usr/bin/env python3
"""Test temporal tools directly."""

import asyncio
import os
import sys
from datetime import datetime, timezone, timedelta
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent.parent / "mcp-server" / "src"))
os.environ['QDRANT_URL'] = 'http://localhost:6333'

from qdrant_client import AsyncQdrantClient
from qdrant_client.models import OrderBy

QDRANT_URL = "http://localhost:6333"

async def test_get_recent():
    """Test get_recent_work functionality directly."""
    print("Testing get_recent_work logic...")
    print("="*60)
    
    client = AsyncQdrantClient(url=QDRANT_URL)
    
    # Import the project resolver
    from project_resolver import ProjectResolver
    from qdrant_client import QdrantClient as SyncQdrantClient
    
    sync_client = SyncQdrantClient(url=QDRANT_URL)
    resolver = ProjectResolver(sync_client)
    
    # Find collections for claude-self-reflect
    project_collections = resolver.find_collections_for_project("claude-self-reflect")
    print(f"Found {len(project_collections)} project collections:")
    for col in project_collections:
        print(f"  - {col}")
    
    # Add reflections
    collections = await client.get_collections()
    reflections_collections = [c.name for c in collections.collections if c.name.startswith('reflections')]
    print(f"\nFound {len(reflections_collections)} reflection collections:")
    for col in reflections_collections:
        print(f"  - {col}")
    
    all_to_search = list(set(project_collections + reflections_collections))
    print(f"\nTotal collections to search: {len(all_to_search)}")
    
    # Get recent work from each collection
    all_chunks = []
    for collection_name in all_to_search:
        try:
            print(f"\nChecking {collection_name}...")
            
            # Get collection info
            info = await client.get_collection(collection_name)
            print(f"  Points: {info.points_count}")
            
            if info.points_count == 0:
                continue
            
            # Try to get recent points with OrderBy
            try:
                results, _ = await client.scroll(
                    collection_name=collection_name,
                    order_by=OrderBy(key="timestamp", direction="desc"),
                    limit=5,
                    with_payload=True
                )
                
                print(f"  Found {len(results)} recent items")
                
                for point in results:
                    chunk_data = {
                        'id': point.id,
                        'collection': collection_name,
                        'timestamp': point.payload.get('timestamp'),
                        'text': point.payload.get('text', ''),
                        'project': point.payload.get('project', ''),
                        'conversation_id': point.payload.get('conversation_id'),
                        'files_analyzed': point.payload.get('files_analyzed', []),
                        'files_edited': point.payload.get('files_edited', []),
                        'tools_used': point.payload.get('tools_used', []),
                        'concepts': point.payload.get('concepts', [])
                    }
                    
                    # Parse timestamp
                    try:
                        ts_str = chunk_data['timestamp']
                        if ts_str:
                            if ts_str.endswith('Z'):
                                ts_str = ts_str.replace('Z', '+00:00')
                            ts = datetime.fromisoformat(ts_str)
                            age = (datetime.now(timezone.utc) - ts).days
                            chunk_data['age_days'] = age
                            print(f"    - {age} days old: {chunk_data['text'][:50]}...")
                    except:
                        pass
                    
                    all_chunks.append(chunk_data)
                    
            except Exception as e:
                print(f"  Error with OrderBy: {e}")
                # Try without OrderBy
                try:
                    results, _ = await client.scroll(
                        collection_name=collection_name,
                        limit=5,
                        with_payload=["timestamp", "text"]
                    )
                    print(f"  Found {len(results)} items (unordered)")
                except Exception as e2:
                    print(f"  Error scrolling: {e2}")
                    
        except Exception as e:
            print(f"  Error accessing collection: {e}")
    
    # Sort all chunks by timestamp
    print(f"\n{'='*60}")
    print(f"SUMMARY: Found {len(all_chunks)} total chunks")
    print(f"{'='*60}")
    
    if all_chunks:
        # Sort by timestamp
        from temporal_utils import TemporalParser
        parser = TemporalParser()
        
        def get_timestamp(chunk):
            try:
                ts_str = chunk.get('timestamp', '')
                if ts_str:
                    return parser._parse_timestamp(ts_str)
            except:
                pass
            return datetime.min.replace(tzinfo=timezone.utc)
        
        all_chunks.sort(key=get_timestamp, reverse=True)
        
        print("\nMost recent conversations:")
        for i, chunk in enumerate(all_chunks[:10]):
            age = chunk.get('age_days', '?')
            text = chunk.get('text', '')[:80]
            col = chunk.get('collection', '')
            print(f"{i+1}. [{age} days old] {col}")
            print(f"   {text}...")
            if chunk.get('files_edited'):
                print(f"   Files: {', '.join(chunk['files_edited'][:3])}")
            print()

if __name__ == "__main__":
    asyncio.run(test_get_recent())