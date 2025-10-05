#!/usr/bin/env python3
"""Check Qdrant collections."""

import os
from qdrant_client import QdrantClient

# Configuration
QDRANT_URL = os.getenv("QDRANT_URL", "http://localhost:6333")

def main():
    """List all collections."""
    client = QdrantClient(url=QDRANT_URL)
    
    # Get all collections
    collections = client.get_collections()
    
    print("Qdrant Collections:")
    print("-" * 60)
    
    voyage_collections = []
    for collection in collections.collections:
        print(f"- {collection.name}")
        if collection.name.endswith("_voyage"):
            voyage_collections.append(collection.name)
    
    print(f"\nFound {len(voyage_collections)} Voyage collections")

if __name__ == "__main__":
    main()