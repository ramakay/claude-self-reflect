#!/usr/bin/env python3
"""Test script to verify local mode is working correctly."""

import os
import sys
sys.path.append(os.path.dirname(os.path.abspath(__file__)))

# Set environment to local mode
os.environ['PREFER_LOCAL_EMBEDDINGS'] = 'true'
os.environ['QDRANT_URL'] = 'http://localhost:6333'
# Clear VOYAGE_KEY to ensure local mode
if 'VOYAGE_KEY' in os.environ:
    del os.environ['VOYAGE_KEY']

from src.embedding_manager import EmbeddingManager
import asyncio
from qdrant_client import AsyncQdrantClient

async def test_local_mode():
    """Test that local mode is properly configured."""

    # Initialize embedding manager
    manager = EmbeddingManager()
    success = manager.initialize()

    print(f"Initialization success: {success}")
    print(f"Model type: {manager.model_type}")
    print(f"Prefer local: {manager.prefer_local}")
    print(f"Has Voyage key: {bool(manager.voyage_key)}")
    print(f"Vector dimension: {manager.get_vector_dimension()}")

    # Test embedding generation
    test_text = "Testing local mode embedding generation"
    embedding = await manager.generate_embedding(test_text)

    if embedding:
        print(f"✅ Generated embedding with {len(embedding)} dimensions")

        # Verify it's 384 dimensions (local)
        if len(embedding) == 384:
            print("✅ Confirmed: Using LOCAL embeddings (384 dimensions)")
        elif len(embedding) == 1024:
            print("❌ ERROR: Still using VOYAGE embeddings (1024 dimensions)")
        else:
            print(f"❓ Unexpected dimension: {len(embedding)}")
    else:
        print("❌ Failed to generate embedding")

    # Check which collection would be used
    collection_name = f"reflections_{manager.model_type}"
    print(f"\nReflections would be stored in: {collection_name}")

    # Check collection in Qdrant
    client = AsyncQdrantClient(url="http://localhost:6333")
    try:
        collection_info = await client.get_collection(collection_name)
        print(f"Collection exists with {collection_info.points_count} points")
    except Exception as e:
        print(f"Collection doesn't exist or error: {e}")
    finally:
        await client.close()

if __name__ == "__main__":
    asyncio.run(test_local_mode())