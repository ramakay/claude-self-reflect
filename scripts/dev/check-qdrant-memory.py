#!/usr/bin/env python3
"""Check Qdrant memory usage and collection statistics."""

import os
from qdrant_client import QdrantClient

# Connect to Qdrant
qdrant_url = os.getenv('QDRANT_URL', 'http://localhost:6333')
client = QdrantClient(url=qdrant_url)

print("Qdrant Collection Statistics")
print("=" * 50)

total_points = 0
total_collections = 0

# Get all collections
collections = client.get_collections().collections

for collection in collections:
    info = client.get_collection(collection.name)
    points = info.points_count
    vectors = info.vectors_count
    
    total_points += points
    total_collections += 1
    
    print(f"\nCollection: {collection.name}")
    print(f"  Points: {points:,}")
    print(f"  Vectors: {vectors:,}" if vectors else "  Vectors: N/A")
    print(f"  Vector size: {info.config.params.vectors.size if hasattr(info.config.params.vectors, 'size') else 'N/A'}")
    
    # Estimate memory usage (rough estimate)
    # Each vector is ~384 dimensions * 4 bytes = 1.5KB
    # Plus metadata overhead
    if hasattr(info.config.params.vectors, 'size'):
        vector_size = info.config.params.vectors.size
        memory_mb = (points * vector_size * 4) / (1024 * 1024)
        print(f"  Estimated vector memory: {memory_mb:.1f} MB")

print("\n" + "=" * 50)
print(f"Total collections: {total_collections}")
print(f"Total points: {total_points:,}")
print(f"\nNote: Actual memory usage includes indexes, metadata, and overhead.")