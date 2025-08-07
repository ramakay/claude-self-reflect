#!/usr/bin/env python3
"""Test cloud mode embedding."""

import os
import sys

# Add the mcp-server/src directory to the Python path
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), 'mcp-server', 'src'))
from utils import normalize_project_name

# Get API key from Docker container
import subprocess
result = subprocess.run(["docker", "exec", "claude-reflection-streaming", "env"], capture_output=True, text=True)
for line in result.stdout.split('\n'):
    if line.startswith('VOYAGE_KEY='):
        voyage_key = line.split('=', 1)[1]
        break
else:
    raise ValueError("No VOYAGE_KEY found in container")

# Configuration
PREFER_LOCAL_EMBEDDINGS = False
VOYAGE_API_KEY = voyage_key

print(f"PREFER_LOCAL_EMBEDDINGS: {PREFER_LOCAL_EMBEDDINGS}")
print(f"VOYAGE_API_KEY exists: {bool(VOYAGE_API_KEY)}")
print(f"VOYAGE_API_KEY: {VOYAGE_API_KEY[:20]}...")

# Test the condition
if PREFER_LOCAL_EMBEDDINGS or not VOYAGE_API_KEY:
    print("Using local embeddings (fastembed)")
else:
    print("Using Voyage AI embeddings")
    
    # Test project name normalization
    project_path = "-Users-ramakrishnanannaswamy-projects-claude-self-reflect"
    normalized = normalize_project_name(project_path)
    print(f"Project path: {project_path}")
    print(f"Normalized: {normalized}")
    
    # Test hash
    import hashlib
    hash_obj = hashlib.md5(normalized.encode())
    expected_hash = hash_obj.hexdigest()[:8]
    print(f"Expected hash: {expected_hash}")
    print(f"Expected collection: conv_{expected_hash}_voyage")
    
    # Test Voyage AI connection
    try:
        import voyageai
        voyage_client = voyageai.Client(api_key=VOYAGE_API_KEY)
        
        # Test embedding
        test_text = "Testing cloud mode collection naming fix"
        result = voyage_client.embed([test_text], model="voyage-3")
        embedding = result.embeddings[0]
        
        print(f"Voyage AI test successful!")
        print(f"Embedding dimension: {len(embedding)}")
        print(f"First 5 values: {embedding[:5]}")
        
    except Exception as e:
        print(f"Error testing Voyage AI: {e}")