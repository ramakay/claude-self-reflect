#!/usr/bin/env python3
"""
Test search performance metrics.
"""

import time
import statistics
from qdrant_client import QdrantClient

print("🧪 Testing Search Performance\n")

client = QdrantClient(url="http://localhost:6333")

# Get collection stats
collections = client.get_collections().collections
print(f"Total collections: {len(collections)}")
total_points = 0

for collection in collections[:5]:  # Sample first 5
    info = client.get_collection(collection.name)
    total_points += info.points_count
    print(f"  {collection.name}: {info.points_count} points")

print(f"\nTotal points sampled: {total_points}")

# Test search performance
print("\n📊 Search Performance Test:")

test_queries = [
    [0.1] * 384,  # Dummy vector for local embeddings
    [0.2] * 384,
    [0.3] * 384,
    [0.4] * 384,
    [0.5] * 384
]

response_times = []

for i, query_vector in enumerate(test_queries, 1):
    # Test on first collection
    collection_name = collections[0].name
    
    start = time.time()
    results = client.search(
        collection_name=collection_name,
        query_vector=query_vector,
        limit=10
    )
    elapsed = (time.time() - start) * 1000  # Convert to ms
    response_times.append(elapsed)
    
    print(f"  Query {i}: {elapsed:.2f}ms ({len(results)} results)")

# Calculate statistics
avg_time = statistics.mean(response_times)
median_time = statistics.median(response_times)
min_time = min(response_times)
max_time = max(response_times)

print(f"\n📈 Performance Summary:")
print(f"  Average: {avg_time:.2f}ms")
print(f"  Median: {median_time:.2f}ms")
print(f"  Min: {min_time:.2f}ms")
print(f"  Max: {max_time:.2f}ms")

# Test cross-collection search
print(f"\n🔄 Cross-Collection Search Test:")

start = time.time()
for collection in collections[:3]:
    client.search(
        collection_name=collection.name,
        query_vector=test_queries[0],
        limit=5
    )
elapsed = (time.time() - start) * 1000

print(f"  3 collections searched: {elapsed:.2f}ms total")
print(f"  Average per collection: {elapsed/3:.2f}ms")

# Check if performance meets targets
print(f"\n✅ Performance Targets:")
if avg_time < 100:
    print(f"  ✅ Average response time < 100ms ({avg_time:.2f}ms)")
else:
    print(f"  ❌ Average response time > 100ms ({avg_time:.2f}ms)")

if max_time < 200:
    print(f"  ✅ Max response time < 200ms ({max_time:.2f}ms)")
else:
    print(f"  ❌ Max response time > 200ms ({max_time:.2f}ms)")