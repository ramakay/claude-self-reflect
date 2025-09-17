#!/usr/bin/env python3
"""Test that ProjectResolver works with sync client."""

import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).parent.parent / 'mcp-server' / 'src'))

from qdrant_client import QdrantClient
from project_resolver import ProjectResolver

# Create sync client (as the fixed server.py now does)
sync_client = QdrantClient(url="http://localhost:6333")

# Create resolver with sync client
resolver = ProjectResolver(sync_client)

# Test projects
test_projects = [
    "claude-self-reflect",
    "cc-enhance",
    "all"
]

print("=== Testing ProjectResolver with Sync Client ===\n")

for project in test_projects:
    print(f"Project: '{project}'")
    try:
        collections = resolver.find_collections_for_project(project)
        print(f"  ✓ Found {len(collections)} collections")
        
        if collections and len(collections) <= 3:
            # Show all if 3 or fewer
            for coll in collections:
                info = sync_client.get_collection(coll)
                print(f"    - {coll}: {info.points_count} points")
        elif collections:
            # Show first 3 if more
            for coll in collections[:3]:
                info = sync_client.get_collection(coll)
                print(f"    - {coll}: {info.points_count} points")
            print(f"    ... and {len(collections) - 3} more")
    except Exception as e:
        print(f"  ✗ Error: {e}")
    print()

# Test that collections actually have vectors
print("\n=== Verifying Vector Integrity ===")
collections = sync_client.get_collections().collections
local_with_vectors = 0
voyage_with_vectors = 0

for coll in collections:
    if coll.name.startswith('conv_'):
        info = sync_client.get_collection(coll.name)
        if info.points_count > 0:
            if coll.name.endswith('_local'):
                local_with_vectors += 1
            elif coll.name.endswith('_voyage'):
                voyage_with_vectors += 1

print(f"Local collections with vectors: {local_with_vectors}")
print(f"Voyage collections with vectors: {voyage_with_vectors}")
print(f"Total collections with vectors: {local_with_vectors + voyage_with_vectors}")