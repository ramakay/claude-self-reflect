#!/usr/bin/env python3
"""Get comprehensive Qdrant statistics for all collections."""

import os
from qdrant_client import QdrantClient
from collections import defaultdict

# Configuration
QDRANT_URL = os.getenv("QDRANT_URL", "http://localhost:6333")

def main():
    """Get detailed statistics for all collections."""
    client = QdrantClient(url=QDRANT_URL)
    
    # Get all collections
    collections = client.get_collections()
    
    total_points = 0
    local_points = 0
    voyage_points = 0
    empty_collections = []
    project_points = defaultdict(int)
    collection_details = []
    
    print("=" * 80)
    print("QDRANT COLLECTION STATISTICS")
    print("=" * 80)
    
    for collection in collections.collections:
        info = client.get_collection(collection.name)
        points = info.points_count
        vectors_config = info.config.params.vectors
        
        # Get vector dimensions
        if hasattr(vectors_config, 'size'):
            dimensions = vectors_config.size
        else:
            # Handle named vectors
            dimensions = list(vectors_config.values())[0].size if vectors_config else 0
        
        collection_details.append({
            'name': collection.name,
            'points': points,
            'dimensions': dimensions,
            'type': 'local' if collection.name.endswith('_local') else 'voyage' if collection.name.endswith('_voyage') else 'other'
        })
        
        total_points += points
        
        if collection.name.endswith('_local'):
            local_points += points
        elif collection.name.endswith('_voyage'):
            voyage_points += points
            
        if points == 0:
            empty_collections.append(collection.name)
            
        # Extract project name from collection name
        if collection.name.startswith('conv_'):
            # Collection names are like conv_<hash>_<type>
            # Map to project based on actual data
            project_points[collection.name] = points
    
    # Sort collections by point count
    collection_details.sort(key=lambda x: x['points'], reverse=True)
    
    print(f"\nSUMMARY:")
    print(f"  Total Collections: {len(collections.collections)}")
    print(f"  Local Collections: {len([c for c in collection_details if c['type'] == 'local'])}")
    print(f"  Voyage Collections: {len([c for c in collection_details if c['type'] == 'voyage'])}")
    print(f"  Empty Collections: {len(empty_collections)}")
    print(f"\nPOINT COUNTS:")
    print(f"  Total Points: {total_points:,}")
    print(f"  Local Points: {local_points:,}")
    print(f"  Voyage Points: {voyage_points:,}")
    
    print(f"\nTOP 10 COLLECTIONS BY POINT COUNT:")
    for i, col in enumerate(collection_details[:10], 1):
        print(f"  {i:2}. {col['name']}: {col['points']:,} points ({col['dimensions']}d)")
    
    if empty_collections:
        print(f"\nEMPTY COLLECTIONS ({len(empty_collections)}):")
        for col in empty_collections[:10]:
            print(f"  - {col}")
        if len(empty_collections) > 10:
            print(f"  ... and {len(empty_collections) - 10} more")
    
    # Check for dimension mismatches
    dimensions_set = set(c['dimensions'] for c in collection_details if c['points'] > 0)
    if len(dimensions_set) > 1:
        print(f"\n⚠️  DIMENSION MISMATCH DETECTED:")
        print(f"  Found {len(dimensions_set)} different dimensions: {dimensions_set}")
        for dim in dimensions_set:
            cols = [c for c in collection_details if c['dimensions'] == dim and c['points'] > 0]
            print(f"  {dim}d: {len(cols)} collections with {sum(c['points'] for c in cols):,} points")
    
    # Compare with MCP reported numbers
    print(f"\n📊 COMPARISON WITH REPORTED NUMBERS:")
    print(f"  MCP Tool Reports: 145/580 conversations (25.0%)")
    print(f"  Status.py Reports: 579/580 files (99.8%)")
    print(f"  Actual Qdrant Points: {total_points:,}")
    print(f"  Config imported-files.json: 682 files")
    
    # Calculate chunk-to-file ratio
    if total_points > 0:
        avg_chunks_per_file = total_points / 579 if 579 > 0 else 0
        print(f"\n📈 METRICS:")
        print(f"  Average chunks per file: {avg_chunks_per_file:.1f}")
        print(f"  Estimated conversations: {total_points} chunks")

if __name__ == "__main__":
    main()