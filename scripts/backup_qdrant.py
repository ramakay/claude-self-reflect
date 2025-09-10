#!/usr/bin/env python3
"""Backup all Qdrant collections."""

import json
import os
from datetime import datetime
from pathlib import Path
from qdrant_client import QdrantClient
from qdrant_client.models import PointStruct

def backup_qdrant():
    """Create a comprehensive backup of all Qdrant collections."""
    
    client = QdrantClient("http://localhost:6333")
    
    # Create backup directory
    backup_dir = Path(f"qdrant_backup_{datetime.now().strftime('%Y%m%d_%H%M%S')}")
    backup_dir.mkdir(exist_ok=True)
    
    # Get all collections
    collections = client.get_collections().collections
    
    print(f"Found {len(collections)} collections to backup")
    
    backup_manifest = {
        "timestamp": datetime.now().isoformat(),
        "collections": [],
        "total_points": 0
    }
    
    for collection in collections:
        collection_name = collection.name
        print(f"Backing up {collection_name}...")
        
        # Get collection info
        info = client.get_collection(collection_name)
        
        # Create collection backup
        collection_backup = {
            "name": collection_name,
            "points_count": info.points_count,
            "vectors_config": str(info.config.params.vectors),  # Convert to string for JSON
            "points": []
        }
        
        # Scroll through all points
        offset = None
        all_points = []
        
        while True:
            records, offset = client.scroll(
                collection_name=collection_name,
                offset=offset,
                limit=100
            )
            
            if not records:
                break
                
            all_points.extend(records)
            
            if offset is None:
                break
        
        # Save points to file
        collection_file = backup_dir / f"{collection_name}.json"
        with open(collection_file, 'w') as f:
            points_data = []
            for point in all_points:
                points_data.append({
                    "id": point.id,
                    "vector": point.vector,
                    "payload": point.payload
                })
            json.dump(points_data, f, indent=2)
        
        collection_backup["points_count"] = len(all_points)
        backup_manifest["collections"].append(collection_backup)
        backup_manifest["total_points"] += len(all_points)
        
        print(f"  - Backed up {len(all_points)} points")
    
    # Save manifest
    manifest_file = backup_dir / "manifest.json"
    with open(manifest_file, 'w') as f:
        json.dump(backup_manifest, f, indent=2)
    
    print(f"\nBackup complete:")
    print(f"  - Directory: {backup_dir}")
    print(f"  - Collections: {len(collections)}")
    print(f"  - Total points: {backup_manifest['total_points']}")
    
    return backup_dir

if __name__ == "__main__":
    backup_qdrant()