#!/usr/bin/env python3
"""Analyze imported-files.json vs actual Qdrant database state."""

import os
import json
from pathlib import Path
from qdrant_client import QdrantClient
from collections import defaultdict

# Configuration
QDRANT_URL = os.getenv("QDRANT_URL", "http://localhost:6333")

def main():
    """Analyze import state discrepancies."""
    client = QdrantClient(url=QDRANT_URL)
    
    # Load imported-files.json
    config_path = Path("/Users/ramakrishnanannaswamy/projects/claude-self-reflect/config/imported-files.json")
    with open(config_path, 'r') as f:
        data = json.load(f)
    
    imported_files = data.get("imported_files", {})
    file_metadata = data.get("file_metadata", {})
    
    print("=" * 80)
    print("IMPORT STATE ANALYSIS")
    print("=" * 80)
    
    # Count total chunks in imported_files
    total_chunks_recorded = 0
    for entry in imported_files.values():
        if isinstance(entry, dict):
            total_chunks_recorded += entry.get("chunks", 0)
        # Some entries might be strings or other types
    
    print(f"\nIMPORTED-FILES.JSON ANALYSIS:")
    print(f"  Total files in imported_files: {len(imported_files)}")
    print(f"  Total chunks recorded: {total_chunks_recorded:,}")
    print(f"  Files in file_metadata: {len(file_metadata)}")
    
    # Get actual Qdrant data
    collections = client.get_collections()
    total_points_actual = 0
    collection_by_project = defaultdict(list)
    
    for collection in collections.collections:
        info = client.get_collection(collection.name)
        total_points_actual += info.points_count
        
        # Try to extract project from collection name
        if collection.name.startswith('conv_'):
            collection_by_project[collection.name].append(info.points_count)
    
    print(f"\nQDRANT ACTUAL STATE:")
    print(f"  Total collections: {len(collections.collections)}")
    print(f"  Total points in Qdrant: {total_points_actual:,}")
    
    print(f"\nDISCREPANCY ANALYSIS:")
    print(f"  Chunks recorded in config: {total_chunks_recorded:,}")
    print(f"  Points in Qdrant: {total_points_actual:,}")
    print(f"  Difference: {total_points_actual - total_chunks_recorded:,}")
    
    # Analyze file paths to understand project distribution
    project_files = defaultdict(list)
    for file_path, info in imported_files.items():
        # Extract project from path
        if "projects-" in file_path:
            parts = file_path.split("projects-")
            if len(parts) > 1:
                project_part = parts[1].split("/")[0]
                chunks = 0
                if isinstance(info, dict):
                    chunks = info.get('chunks', 0)
                project_files[project_part].append({
                    'path': file_path,
                    'chunks': chunks
                })
    
    print(f"\nPROJECT BREAKDOWN:")
    for project, files in sorted(project_files.items(), key=lambda x: len(x[1]), reverse=True)[:10]:
        total_chunks = sum(f['chunks'] for f in files)
        print(f"  {project}: {len(files)} files, {total_chunks:,} chunks")
    
    # Check for files that might be partially imported
    partially_imported = []
    for file_path, metadata in file_metadata.items():
        if isinstance(metadata, dict):
            position = metadata.get("position", 0)
            if position > 0 and file_path not in imported_files:
                partially_imported.append((file_path, position))
    
    if partially_imported:
        print(f"\nPARTIALLY IMPORTED FILES: {len(partially_imported)}")
        for path, pos in partially_imported[:5]:
            print(f"  {Path(path).name}: position {pos}")
    
    # Special collections analysis
    special_collections = [c for c in collections.collections if not c.name.startswith('conv_')]
    if special_collections:
        print(f"\nSPECIAL COLLECTIONS:")
        for col in special_collections:
            info = client.get_collection(col.name)
            print(f"  {col.name}: {info.points_count:,} points")
    
    # Check for the mysterious ws- collection with 17K points
    ws_collections = [c for c in collections.collections if c.name.startswith('ws-')]
    if ws_collections:
        print(f"\n⚠️  FOUND WORKSPACE COLLECTION:")
        for col in ws_collections:
            info = client.get_collection(col.name)
            print(f"  {col.name}: {info.points_count:,} points")
            print(f"  This appears to be a separate workspace/test collection not tracked in imported-files.json")

if __name__ == "__main__":
    main()