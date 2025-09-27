#!/usr/bin/env python3
"""Test ProjectResolver to see if it's finding collections correctly."""

import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).parent.parent / 'mcp-server' / 'src'))

from qdrant_client import QdrantClient
from project_resolver import ProjectResolver

# Connect to Qdrant
client = QdrantClient(url="http://localhost:6333")

# Create resolver
resolver = ProjectResolver(client)

# Test projects
test_projects = [
    "claude-self-reflect",
    "memento",  
    "cc-enhance",
    "all"
]

print("=== Testing ProjectResolver ===\n")

for project in test_projects:
    print(f"Project: '{project}'")
    collections = resolver.find_collections_for_project(project)
    print(f"  Found {len(collections)} collections")
    
    if collections:
        # Show first 3 collections
        for coll in collections[:3]:
            try:
                info = client.get_collection(coll)
                suffix = "_local" if coll.endswith("_local") else "_voyage"
                print(f"    - {coll}: {info.points_count} points ({suffix})")
            except:
                print(f"    - {coll}: <error getting info>")
    else:
        print("    - No collections found!")
    print()

# Also test the normalization directly
print("\n=== Testing Direct Normalization ===")
from shared.normalization import normalize_project_name
import hashlib

test_paths = [
    str(Path.home() / "projects" / "claude-self-reflect"),
    str(Path.home() / "projects" / "memento"),
    str(Path.home() / "projects" / "cc-enhance")
]

for path in test_paths:
    normalized = normalize_project_name(path)
    name_hash = hashlib.md5(normalized.encode()).hexdigest()[:8]
    collection_local = f"conv_{name_hash}_local"
    collection_voyage = f"conv_{name_hash}_voyage"
    
    print(f"Path: {path}")
    print(f"  Normalized: {normalized}")
    print(f"  Hash: {name_hash}")
    print(f"  Expected collections:")
    print(f"    - {collection_local}")
    print(f"    - {collection_voyage}")
    
    # Check if these exist
    all_collections = [c.name for c in client.get_collections().collections]
    if collection_local in all_collections:
        info = client.get_collection(collection_local)
        print(f"    ✓ {collection_local} exists with {info.points_count} points")
    else:
        print(f"    ✗ {collection_local} not found")
    
    if collection_voyage in all_collections:
        info = client.get_collection(collection_voyage)
        print(f"    ✓ {collection_voyage} exists with {info.points_count} points")
    else:
        print(f"    ✗ {collection_voyage} not found")
    print()