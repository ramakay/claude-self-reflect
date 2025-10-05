#!/usr/bin/env python3
"""Add timestamp indexes to all collections for OrderBy support."""

import asyncio
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).parent.parent))

from qdrant_client import AsyncQdrantClient
from qdrant_client.models import PayloadSchemaType, OrderBy

QDRANT_URL = os.getenv("QDRANT_URL", "http://localhost:6333")

async def add_timestamp_indexes():
    """Add timestamp indexes to all collections that need them."""
    client = AsyncQdrantClient(url=QDRANT_URL)
    
    print("Adding timestamp indexes for temporal query support...")
    print("="*60)
    
    # Get all collections
    collections = await client.get_collections()
    total = len(collections.collections)
    print(f"Found {total} collections")
    
    success_count = 0
    skip_count = 0
    error_count = 0
    
    for i, col in enumerate(collections.collections, 1):
        col_name = col.name
        print(f"\n[{i}/{total}] Processing {col_name}...")
        
        try:
            # Check if collection has points
            info = await client.get_collection(col_name)
            if info.points_count == 0:
                print(f"  ⏭️  Skipped (empty collection)")
                skip_count += 1
                continue
            
            # Check if timestamp field exists
            points, _ = await client.scroll(
                collection_name=col_name,
                limit=1,
                with_payload=["timestamp"]
            )
            
            if not points or not points[0].payload.get('timestamp'):
                print(f"  ⏭️  Skipped (no timestamp field)")
                skip_count += 1
                continue
            
            # Try to use OrderBy to check if index exists
            try:
                await client.scroll(
                    collection_name=col_name,
                    order_by=OrderBy(key="timestamp", direction="desc"),
                    limit=1
                )
                print(f"  ✅ Already has timestamp index")
                skip_count += 1
            except Exception as e:
                if "No range index" in str(e):
                    # Need to create index
                    print(f"  🔧 Creating timestamp index...")
                    try:
                        await client.create_payload_index(
                            collection_name=col_name,
                            field_name="timestamp",
                            field_schema=PayloadSchemaType.DATETIME
                        )
                        print(f"  ✅ Index created successfully")
                        success_count += 1
                    except Exception as create_error:
                        print(f"  ❌ Failed to create index: {create_error}")
                        error_count += 1
                else:
                    print(f"  ⚠️  Unexpected error: {e}")
                    error_count += 1
                    
        except Exception as e:
            print(f"  ❌ Error: {e}")
            error_count += 1
    
    print("\n" + "="*60)
    print("SUMMARY")
    print("="*60)
    print(f"✅ Indexes created: {success_count}")
    print(f"⏭️  Skipped: {skip_count}")
    print(f"❌ Errors: {error_count}")
    print(f"📊 Total collections: {total}")
    
    # Verify temporal queries work
    if success_count > 0:
        print("\n" + "="*60)
        print("VERIFYING TEMPORAL QUERIES")
        print("="*60)
        
        # Find a collection with data to test
        test_collection = None
        for col in collections.collections:
            try:
                info = await client.get_collection(col.name)
                if info.points_count > 100:  # Find one with decent amount of data
                    test_collection = col.name
                    break
            except:
                pass
        
        if test_collection:
            print(f"Testing on {test_collection}...")
            try:
                # Test OrderBy
                results, _ = await client.scroll(
                    collection_name=test_collection,
                    order_by=OrderBy(key="timestamp", direction="desc"),
                    limit=3,
                    with_payload=["timestamp", "text"]
                )
                
                print(f"✅ OrderBy works! Found {len(results)} recent conversations:")
                for r in results:
                    ts = r.payload.get('timestamp', 'N/A')
                    text = r.payload.get('text', '')[:60] + '...'
                    print(f"  - {ts}: {text}")
                    
            except Exception as e:
                print(f"❌ OrderBy test failed: {e}")

if __name__ == "__main__":
    asyncio.run(add_timestamp_indexes())