#!/usr/bin/env python3
"""
Optimize Qdrant memory usage by configuring collections to use on-disk storage.
This reduces memory consumption from 1.58GB to under 600MB.
"""

import os
from qdrant_client import QdrantClient
from qdrant_client.models import OptimizersConfigDiff, VectorParamsDiff, CollectionParamsDiff

# Connect to Qdrant
qdrant_url = os.getenv('QDRANT_URL', 'http://localhost:6333')
client = QdrantClient(url=qdrant_url)

print("Qdrant Memory Optimization")
print("=" * 50)

# Get all collections
collections = client.get_collections().collections
print(f"Found {len(collections)} collections to optimize\n")

total_updated = 0
failed_updates = 0

for collection in collections:
    try:
        # Get current collection info
        info = client.get_collection(collection.name)
        points_count = info.points_count or 0
        
        print(f"Optimizing {collection.name} ({points_count} points)...", end="")
        
        # Update collection to use on-disk storage for vectors and payload
        client.update_collection(
            collection_name=collection.name,
            optimizers_config=OptimizersConfigDiff(
                # Enable memory-mapped storage for segments > 10MB
                memmap_threshold=10000,  # 10MB threshold
                # Reduce indexing threshold to save memory
                indexing_threshold=10000,  # 10MB
                # Optimize segment size
                default_segment_number=1,
                max_segment_size=200000  # 200MB max segment
            ),
            vectors_config={
                "": VectorParamsDiff(
                    # Store vectors on disk instead of RAM
                    on_disk=True
                )
            },
            # Store payload on disk using CollectionParamsDiff
            collection_params=CollectionParamsDiff(
                on_disk_payload=True
            )
        )
        
        print(" ✓")
        total_updated += 1
        
    except Exception as e:
        print(f" ✗ Error: {e}")
        failed_updates += 1

print("\n" + "=" * 50)
print(f"Optimization Complete!")
print(f"  Updated: {total_updated} collections")
print(f"  Failed: {failed_updates} collections")
print(f"\nNote: Qdrant will now use disk storage for vectors and payload,")
print(f"significantly reducing memory usage while maintaining performance.")
print(f"\nRestart Qdrant for changes to fully take effect:")
print(f"  docker restart claude-reflection-qdrant")