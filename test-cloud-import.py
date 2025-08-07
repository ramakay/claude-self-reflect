#!/usr/bin/env python3
"""Test cloud mode import directly."""

import os
import sys
import subprocess
import json
from datetime import datetime

# Add the mcp-server/src directory to the Python path  
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), 'mcp-server', 'src'))
from utils import normalize_project_name

from qdrant_client import QdrantClient
from qdrant_client.models import VectorParams, Distance, PointStruct

# Get API key from Docker container
result = subprocess.run(["docker", "exec", "claude-reflection-streaming", "env"], capture_output=True, text=True)
for line in result.stdout.split('\n'):
    if line.startswith('VOYAGE_KEY='):
        voyage_key = line.split('=', 1)[1]
        break
else:
    raise ValueError("No VOYAGE_KEY found in container")

# Initialize Voyage client
import voyageai
voyage_client = voyageai.Client(api_key=voyage_key)

# Initialize Qdrant client
qdrant_client = QdrantClient(url="http://localhost:6333")

# Test data
project_path = "-Users-ramakrishnanannaswamy-projects-claude-self-reflect"
normalized_name = normalize_project_name(project_path)
import hashlib
hash_obj = hashlib.md5(normalized_name.encode())
collection_name = f"conv_{hash_obj.hexdigest()[:8]}_voyage"

print(f"Collection name: {collection_name}")

# Create collection if it doesn't exist
try:
    qdrant_client.get_collection(collection_name)
    print("Collection already exists")
except:
    print("Creating new collection...")
    qdrant_client.create_collection(
        collection_name=collection_name,
        vectors_config=VectorParams(size=1024, distance=Distance.COSINE)
    )

# Test conversation
test_conversation = {
    "uuid": "cloud-test-validation",
    "name": "Cloud Mode Validation",
    "messages": [
        {
            "role": "human", 
            "content": "Testing Voyage AI cloud embeddings for collection naming fix validation"
        },
        {
            "role": "assistant",
            "content": [{"type": "text", "text": "This validates that conv_7f6df0fc_voyage collection is used correctly in cloud mode"}]
        }
    ]
}

# Process messages and create embeddings
points = []
for i, message in enumerate(test_conversation["messages"]):
    if message["role"] == "human":
        content = message["content"]
    else:
        content = message["content"][0]["text"] if isinstance(message["content"], list) else message["content"]
    
    # Generate embedding
    result = voyage_client.embed([content], model="voyage-3")
    embedding = result.embeddings[0]
    
    # Create point (use integer ID)
    point = PointStruct(
        id=i + 1000,  # Use integer ID
        vector=embedding,
        payload={
            "conversation_uuid": test_conversation["uuid"],
            "conversation_name": test_conversation["name"],
            "message_role": message["role"],
            "message_content": content,
            "created_at": datetime.now().isoformat()
        }
    )
    points.append(point)

# Upload to Qdrant
qdrant_client.upsert(
    collection_name=collection_name,
    points=points
)

print(f"Successfully uploaded {len(points)} points to {collection_name}")

# Verify
collection_info = qdrant_client.get_collection(collection_name)
print(f"Collection now has {collection_info.points_count} points")
print(f"Vector size: {collection_info.config.params.vectors.size}")