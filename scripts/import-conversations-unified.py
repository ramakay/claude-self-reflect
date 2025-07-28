#!/usr/bin/env python3
"""
Unified import script that supports both local and Voyage AI embeddings.
"""

import os
import sys
import json
import glob
import hashlib
from datetime import datetime
from typing import List, Dict, Any
import logging
from pathlib import Path

from qdrant_client import QdrantClient
from qdrant_client.models import (
    VectorParams, Distance, PointStruct,
    Filter, FieldCondition, MatchValue
)

from tenacity import (
    retry,
    stop_after_attempt,
    wait_random_exponential,
)

# Configuration
QDRANT_URL = os.getenv("QDRANT_URL", "http://localhost:6333")
LOGS_DIR = os.getenv("LOGS_DIR", "/logs")
STATE_FILE = os.getenv("STATE_FILE", "/config/imported-files.json")
BATCH_SIZE = int(os.getenv("BATCH_SIZE", "100"))
PREFER_LOCAL_EMBEDDINGS = os.getenv("PREFER_LOCAL_EMBEDDINGS", "false").lower() == "true"
VOYAGE_API_KEY = os.getenv("VOYAGE_KEY")

# Set up logging
logging.basicConfig(level=logging.INFO, format='%(asctime)s - %(levelname)s - %(message)s')
logger = logging.getLogger(__name__)

# Initialize embedding provider
embedding_provider = None
embedding_dimension = None
collection_suffix = None

if PREFER_LOCAL_EMBEDDINGS or not VOYAGE_API_KEY:
    # Use local embeddings
    logger.info("Using local embeddings (fastembed)")
    from fastembed import TextEmbedding
    embedding_provider = TextEmbedding("sentence-transformers/all-MiniLM-L6-v2")
    embedding_dimension = 384
    collection_suffix = "_local"
else:
    # Use Voyage AI
    logger.info("Using Voyage AI embeddings")
    import voyageai
    voyage_client = voyageai.Client(api_key=VOYAGE_API_KEY)
    embedding_dimension = 1024
    collection_suffix = "_voyage"

# Initialize Qdrant client
client = QdrantClient(url=QDRANT_URL)


def log_retry_state(retry_state):
    print(f"Retrying function '{retry_state.fn.__name__}' for the {retry_state.attempt_number} time.")
    print(f"----> Waiting for {retry_state.next_action.sleep} seconds before next attempt.")

@retry(wait=wait_random_exponential(multiplier=2, min=30, max=120), stop=stop_after_attempt(6), before_sleep=log_retry_state)
def embed_with_backoff(**kwargs):
    return voyage_client.embed(**kwargs)

def generate_embeddings(texts: List[str]) -> List[List[float]]:
    """Generate embeddings for a list of texts."""
    if PREFER_LOCAL_EMBEDDINGS or not VOYAGE_API_KEY:
        # Local embeddings using FastEmbed
        embeddings = list(embedding_provider.passage_embed(texts))
        return [embedding.tolist() for embedding in embeddings]
    else:
        # Voyage AI embeddings
        result = embed_with_backoff(
            texts=texts,
            model="voyage-3-large",
            input_type="document"
        )
        return result.embeddings

def chunk_conversation(messages: List[Dict[str, Any]], chunk_size: int = 10) -> List[Dict[str, Any]]:
    """Chunk conversation into smaller segments."""
    chunks = []
    
    for i in range(0, len(messages), chunk_size):
        chunk_messages = messages[i:i + chunk_size]
        
        # Extract text content
        texts = []
        for msg in chunk_messages:
            role = msg.get("role", "unknown")
            content = msg.get("content", "")
            
            if isinstance(content, list):
                # Handle structured content
                text_parts = []
                for item in content:
                    if isinstance(item, dict) and item.get("type") == "text":
                        text_parts.append(item.get("text", ""))
                    elif isinstance(item, str):
                        text_parts.append(item)
                content = " ".join(text_parts)
            
            if content:
                texts.append(f"{role.upper()}: {content}")
        
        if texts:
            chunks.append({
                "text": "\n".join(texts),
                "messages": chunk_messages,
                "chunk_index": i // chunk_size,
                "start_role": chunk_messages[0].get("role", "unknown") if chunk_messages else "unknown"
            })
    
    return chunks

def import_project(project_path: Path, collection_name: str) -> int:
    """Import all conversations from a project."""
    jsonl_files = list(project_path.glob("*.jsonl"))
    
    if not jsonl_files:
        logger.warning(f"No JSONL files found in {project_path}")
        return 0
    
    # Check if collection exists
    collections = client.get_collections().collections
    if collection_name not in [c.name for c in collections]:
        logger.info(f"Creating collection: {collection_name}")
        client.create_collection(
            collection_name=collection_name,
            vectors_config=VectorParams(
                size=embedding_dimension,
                distance=Distance.COSINE
            )
        )
    
    total_chunks = 0
    
    for jsonl_file in jsonl_files:
        logger.info(f"Processing file: {jsonl_file.name}")
        try:
            # Read JSONL file and extract messages
            messages = []
            created_at = None
            
            with open(jsonl_file, 'r', encoding='utf-8') as f:
                for line_num, line in enumerate(f, 1):
                    line = line.strip()
                    if not line:
                        continue
                    
                    try:
                        data = json.loads(line)
                        
                        # Extract timestamp from first message
                        if created_at is None and 'timestamp' in data:
                            created_at = data.get('timestamp')
                        
                        # Skip non-message lines (summaries, etc.)
                        if data.get('type') == 'summary':
                            continue
                            
                        # Extract message if present
                        if 'message' in data and data['message']:
                            msg = data['message']
                            if msg.get('role') and msg.get('content'):
                                # Handle content that's an array of objects
                                content = msg['content']
                                if isinstance(content, list):
                                    text_parts = []
                                    for item in content:
                                        if isinstance(item, dict) and item.get('type') == 'text':
                                            text_parts.append(item.get('text', ''))
                                        elif isinstance(item, str):
                                            text_parts.append(item)
                                    content = '\n'.join(text_parts)
                                
                                if content:
                                    messages.append({
                                        'role': msg['role'],
                                        'content': content
                                    })
                    except json.JSONDecodeError:
                        logger.debug(f"Skipping invalid JSON at line {line_num}")
                    except Exception as e:
                        logger.error(f"Error processing line {line_num}: {e}")
            
            if not messages:
                continue
            
            # Extract metadata
            if created_at is None:
                created_at = datetime.now().isoformat()
            conversation_id = jsonl_file.stem
            
            # Chunk the conversation
            chunks = chunk_conversation(messages)
            
            if not chunks:
                continue
            
            # Process in batches
            for batch_start in range(0, len(chunks), BATCH_SIZE):
                batch = chunks[batch_start:batch_start + BATCH_SIZE]
                texts = [chunk["text"] for chunk in batch]
                
                # Generate embeddings
                embeddings = generate_embeddings(texts)
                
                # Create points
                points = []
                for chunk, embedding in zip(batch, embeddings):
                    point_id = hashlib.md5(
                        f"{conversation_id}_{chunk['chunk_index']}".encode()
                    ).hexdigest()[:16]
                    
                    points.append(PointStruct(
                        id=int(point_id, 16) % (2**63),  # Convert to valid integer ID
                        vector=embedding,
                        payload={
                            "text": chunk["text"],
                            "conversation_id": conversation_id,
                            "chunk_index": chunk["chunk_index"],
                            "timestamp": created_at,
                            "project": project_path.name,
                            "start_role": chunk["start_role"]
                        }
                    ))
                
                # Upload to Qdrant
                client.upsert(
                    collection_name=collection_name,
                    points=points
                )
                
                total_chunks += len(points)
            
            logger.info(f"Imported {len(chunks)} chunks from {jsonl_file.name}")
            
        except Exception as e:
            logger.error(f"Failed to import {jsonl_file}: {e}")
            import traceback
            logger.error(traceback.format_exc())
    
    return total_chunks

def main():
    """Main import function."""
    logs_path = Path(LOGS_DIR)
    
    if not logs_path.exists():
        logger.error(f"Logs directory not found: {LOGS_DIR}")
        return
    
    # Find all project directories
    project_dirs = [d for d in logs_path.iterdir() if d.is_dir()]
    
    if not project_dirs:
        logger.warning("No project directories found")
        return
    
    logger.info(f"Found {len(project_dirs)} projects to import")
    
    # Import each project
    total_imported = 0
    for project_dir in project_dirs:
        # Create collection name from project path
        collection_name = f"conv_{hashlib.md5(project_dir.name.encode()).hexdigest()[:8]}{collection_suffix}"
        
        logger.info(f"Importing project: {project_dir.name} -> {collection_name}")
        chunks = import_project(project_dir, collection_name)
        total_imported += chunks
        logger.info(f"Imported {chunks} chunks from {project_dir.name}")
    
    logger.info(f"Import complete! Total chunks imported: {total_imported}")

if __name__ == "__main__":
    main()