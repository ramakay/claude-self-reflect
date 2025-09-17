#!/usr/bin/env python3
"""Debug script to test import and check what's happening with embeddings."""

import sys
import os
from pathlib import Path
import json

# Add scripts directory to path
sys.path.insert(0, str(Path(__file__).parent))

# Load environment
from dotenv import load_dotenv
env_path = Path(__file__).parent.parent / '.env'
if env_path.exists():
    load_dotenv(env_path)
    print(f"✓ Loaded .env from {env_path}")

# Check environment variables
print("\n=== Environment Configuration ===")
print(f"PREFER_LOCAL_EMBEDDINGS: {os.getenv('PREFER_LOCAL_EMBEDDINGS', 'not set')}")
print(f"VOYAGE_KEY: {'set' if os.getenv('VOYAGE_KEY') else 'not set'}")
print(f"QDRANT_URL: {os.getenv('QDRANT_URL', 'not set')}")

# Test embedding provider initialization
print("\n=== Testing Embedding Provider ===")
PREFER_LOCAL_EMBEDDINGS = os.getenv("PREFER_LOCAL_EMBEDDINGS", "true").lower() == "true"
VOYAGE_API_KEY = os.getenv("VOYAGE_KEY")

print(f"PREFER_LOCAL_EMBEDDINGS evaluated to: {PREFER_LOCAL_EMBEDDINGS}")
print(f"VOYAGE_API_KEY is: {'present' if VOYAGE_API_KEY else 'absent'}")

# Initialize embedding provider as the script does
embedding_provider = None
embedding_dimension = None
collection_suffix = None

if PREFER_LOCAL_EMBEDDINGS or not VOYAGE_API_KEY:
    print("→ Using LOCAL embeddings (fastembed)")
    try:
        from fastembed import TextEmbedding
        embedding_provider = TextEmbedding(model_name="sentence-transformers/all-MiniLM-L6-v2")
        embedding_dimension = 384
        collection_suffix = "local"
        print(f"✓ FastEmbed initialized: dimension={embedding_dimension}, suffix={collection_suffix}")
    except Exception as e:
        print(f"✗ Failed to initialize FastEmbed: {e}")
        sys.exit(1)
else:
    print("→ Using VOYAGE embeddings")
    try:
        import voyageai
        embedding_provider = voyageai.Client(api_key=VOYAGE_API_KEY)
        embedding_dimension = 1024
        collection_suffix = "voyage"
        print(f"✓ Voyage initialized: dimension={embedding_dimension}, suffix={collection_suffix}")
    except Exception as e:
        print(f"✗ Failed to initialize Voyage: {e}")
        sys.exit(1)

# Test embedding generation
print("\n=== Testing Embedding Generation ===")

def generate_embeddings(texts):
    """Generate embeddings for texts - mimics the actual function."""
    if PREFER_LOCAL_EMBEDDINGS:
        print(f"Generating LOCAL embeddings for {len(texts)} text(s)")
        embeddings = list(embedding_provider.passage_embed(texts))
        return [emb.tolist() if hasattr(emb, 'tolist') else emb for emb in embeddings]
    else:
        print(f"Generating VOYAGE embeddings for {len(texts)} text(s)")
        response = embedding_provider.embed(texts, model="voyage-3")
        return response.embeddings

test_texts = ["This is a test sentence for embedding generation."]
try:
    embeddings = generate_embeddings(test_texts)
    if embeddings and embeddings[0]:
        print(f"✓ Generated embedding with dimension: {len(embeddings[0])}")
        print(f"✓ First 3 values: {embeddings[0][:3]}")
        print(f"✓ Non-zero values: {sum(1 for x in embeddings[0] if x != 0)}/{len(embeddings[0])}")
    else:
        print("✗ Empty embeddings generated!")
except Exception as e:
    print(f"✗ Error generating embeddings: {e}")
    import traceback
    traceback.print_exc()

# Check Qdrant connection and collections
print("\n=== Checking Qdrant ===")
try:
    from qdrant_client import QdrantClient
    client = QdrantClient(url=os.getenv("QDRANT_URL", "http://localhost:6333"), timeout=5)
    collections = client.get_collections().collections
    print(f"✓ Connected to Qdrant, found {len(collections)} collections")
    
    # Check a sample collection
    if collections:
        sample = collections[0]
        info = client.get_collection(sample.name)
        print(f"Sample collection: {sample.name}")
        print(f"  - Points count: {info.points_count}")
        print(f"  - Vectors count: {info.vectors_count}")
        print(f"  - Vector dimension: {info.config.params.vectors.size if info.config.params.vectors else 'N/A'}")
except Exception as e:
    print(f"✗ Qdrant error: {e}")

print("\n=== Import Script Would Use ===")
print(f"Embeddings: {'LOCAL (FastEmbed)' if PREFER_LOCAL_EMBEDDINGS else 'VOYAGE'}")
print(f"Dimension: {embedding_dimension}")
print(f"Collection suffix: _{collection_suffix}")