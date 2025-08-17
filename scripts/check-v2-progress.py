#!/usr/bin/env python3
"""Quick v2 migration progress check."""

import asyncio
import os
from qdrant_client import AsyncQdrantClient, models

async def check_progress():
    client = AsyncQdrantClient(url=os.getenv("QDRANT_URL", "http://localhost:6333"))
    
    collections = await client.get_collections()
    
    total_v1 = 0
    total_v2 = 0
    total_collections = 0
    
    for coll in collections.collections:
        if not coll.name.startswith("conv_"):
            continue
        
        total_collections += 1
        
        try:
            # Count v1 chunks
            v1_count = await client.count(
                collection_name=coll.name,
                count_filter=models.Filter(
                    must_not=[
                        models.FieldCondition(
                            key="chunking_version",
                            match=models.MatchValue(value="v2")
                        )
                    ]
                )
            )
            
            # Count v2 chunks  
            v2_count = await client.count(
                collection_name=coll.name,
                count_filter=models.Filter(
                    must=[
                        models.FieldCondition(
                            key="chunking_version",
                            match=models.MatchValue(value="v2")
                        )
                    ]
                )
            )
            
            total_v1 += v1_count.count
            total_v2 += v2_count.count
            
            if v1_count.count > 0:
                print(f"  {coll.name}: {v1_count.count} v1, {v2_count.count} v2")
                
        except Exception as e:
            print(f"  Error checking {coll.name}: {e}")
    
    print(f"\n📊 MIGRATION PROGRESS:")
    print(f"  Collections: {total_collections}")
    print(f"  V1 chunks remaining: {total_v1:,}")
    print(f"  V2 chunks created: {total_v2:,}")
    print(f"  Total chunks: {total_v1 + total_v2:,}")
    
    if total_v1 + total_v2 > 0:
        progress = (total_v2 / (total_v1 + total_v2)) * 100
        print(f"  Progress: {progress:.1f}%")
        
        if total_v1 == 0:
            print("\n✅ 100% MIGRATION COMPLETE!")
        else:
            print(f"\n⏳ {total_v1:,} chunks still need migration")

asyncio.run(check_progress())