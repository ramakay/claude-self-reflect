#!/usr/bin/env python3
"""Delete all conversation collections for fresh start."""

from qdrant_client import QdrantClient

def delete_conversation_collections():
    """Delete all conversation collections, keeping only reflections."""
    
    client = QdrantClient("http://localhost:6333")
    
    # Get all collections
    collections = client.get_collections().collections
    
    deleted = []
    kept = []
    
    for collection in collections:
        name = collection.name
        
        # Keep reflections and workspace collections
        if name in ["reflections_local"] or name.startswith("ws-"):
            kept.append(name)
            print(f"Keeping: {name}")
        # Delete conversation collections
        elif name.startswith("conv_"):
            try:
                client.delete_collection(name)
                deleted.append(name)
                print(f"Deleted: {name}")
            except Exception as e:
                print(f"Failed to delete {name}: {e}")
        else:
            kept.append(name)
            print(f"Keeping: {name}")
    
    print(f"\nSummary:")
    print(f"  - Deleted: {len(deleted)} collections")
    print(f"  - Kept: {len(kept)} collections")
    
    return deleted, kept

if __name__ == "__main__":
    delete_conversation_collections()