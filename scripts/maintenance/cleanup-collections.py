#!/usr/bin/env python3
"""
Clean up Qdrant collections based on current embedding mode.
Users use either local (FastEmbed) OR cloud (Voyage AI) embeddings, not both.
This script removes collections from the inactive mode and empty collections.
"""

import os
import sys
from qdrant_client import QdrantClient
from datetime import datetime
import time

# Connect to Qdrant
qdrant_url = os.getenv('QDRANT_URL', 'http://localhost:6333')

# Check current embedding mode from environment
prefer_local = os.getenv('PREFER_LOCAL_EMBEDDINGS', 'true').lower() == 'true'
current_mode = 'local' if prefer_local else 'voyage'
inactive_mode = 'voyage' if prefer_local else 'local'

print("Qdrant Collection Cleanup")
print("=" * 50)
print(f"Current embedding mode: {current_mode.upper()}")
print(f"Will remove: {inactive_mode.upper()} collections and empty collections")
print()

# Wait for Qdrant to be available
max_retries = 30
for i in range(max_retries):
    try:
        client = QdrantClient(url=qdrant_url, timeout=5)
        collections = client.get_collections().collections
        print(f"Connected! Found {len(collections)} total collections")
        break
    except Exception as e:
        if i < max_retries - 1:
            print(f"Waiting for Qdrant... ({i+1}/{max_retries})")
            time.sleep(2)
        else:
            print(f"ERROR: Cannot connect to Qdrant: {e}")
            sys.exit(1)

# Categorize collections
local_collections = []
voyage_collections = []
unknown_collections = []
collection_info = {}

for collection in collections:
    name = collection.name
    collection_info[name] = collection
    
    if name.endswith('_local'):
        local_collections.append(name)
    elif name.endswith('_voyage'):
        voyage_collections.append(name)
    else:
        unknown_collections.append(name)

print(f"\nCollection breakdown:")
print(f"  Local collections: {len(local_collections)}")
print(f"  Voyage collections: {len(voyage_collections)}")
print(f"  Unknown collections: {len(unknown_collections)}")

# Determine what to delete based on mode
to_delete = []
to_keep = []

# Remove collections from inactive mode
if current_mode == 'local':
    to_delete.extend(voyage_collections)
    to_keep.extend(local_collections)
    print(f"\nWill remove {len(voyage_collections)} Voyage collections (using Local mode)")
else:
    to_delete.extend(local_collections)
    to_keep.extend(voyage_collections)
    print(f"\nWill remove {len(local_collections)} Local collections (using Voyage mode)")

# Keep unknown collections (might be test collections)
to_keep.extend(unknown_collections)

# Also check for empty collections
print("\nChecking for empty collections...")
empty_collections = []
for collection_name in to_keep:
    try:
        info = client.get_collection(collection_name)
        if info.points_count == 0:
            empty_collections.append(collection_name)
            print(f"Empty collection: {collection_name}")
    except:
        pass

# Add empty collections to delete list
to_delete.extend(empty_collections)
to_keep = [c for c in to_keep if c not in empty_collections]

print("\n" + "=" * 50)
print(f"Collections to keep: {len(to_keep)}")
print(f"Collections to delete: {len(to_delete)}")

if len(to_delete) == 0:
    print("Nothing to delete!")
    sys.exit(0)

# Ask for confirmation
print("\nThis will DELETE the following collections:")
for name in to_delete[:10]:  # Show first 10
    print(f"  - {name}")
if len(to_delete) > 10:
    print(f"  ... and {len(to_delete) - 10} more")

response = input("\nProceed with deletion? (yes/no): ")
if response.lower() != 'yes':
    print("Aborted.")
    sys.exit(0)

# Delete collections
print("\nDeleting collections...")
deleted = 0
failed = 0

for collection_name in to_delete:
    try:
        client.delete_collection(collection_name)
        print(f"Deleted: {collection_name}")
        deleted += 1
    except Exception as e:
        print(f"Failed to delete {collection_name}: {e}")
        failed += 1

print("\n" + "=" * 50)
print(f"Cleanup Complete!")
print(f"  Deleted: {deleted} collections")
print(f"  Failed: {failed} collections")
print(f"  Remaining: {len(to_keep)} collections")
print(f"\nRestart Qdrant for optimal performance:")
print(f"  docker restart claude-reflection-qdrant")