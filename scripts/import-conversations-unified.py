#!/usr/bin/env python3
"""
Streaming importer with true line-by-line processing to prevent OOM.
Processes JSONL files without loading entire file into memory.
"""

import json
import os
import sys
import hashlib
import gc
from pathlib import Path
from datetime import datetime
from typing import List, Dict, Any, Optional
import logging

# Add the project root to the Python path
project_root = Path(__file__).parent.parent
sys.path.insert(0, str(project_root))

from qdrant_client import QdrantClient
from qdrant_client.models import PointStruct, Distance, VectorParams

# Set up logging
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

# Environment variables
QDRANT_URL = os.getenv("QDRANT_URL", "http://localhost:6333")

# Robust cross-platform state file resolution
def get_default_state_file():
    """Determine the default state file location with cross-platform support."""
    from pathlib import Path
    
    # Check if we're in Docker (more reliable than just checking /config)
    docker_indicators = [
        Path("/.dockerenv").exists(),  # Docker creates this file
        os.path.exists("/config") and os.access("/config", os.W_OK)  # Mounted config dir with write access
    ]
    
    if any(docker_indicators):
        return "/config/imported-files.json"
    
    # Use pathlib for cross-platform home directory path
    home_state = Path.home() / ".claude-self-reflect" / "config" / "imported-files.json"
    return str(home_state)

# Get state file path with env override support
env_state = os.getenv("STATE_FILE")
if env_state:
    # Normalize any user-provided path to absolute
    from pathlib import Path
    STATE_FILE = str(Path(env_state).expanduser().resolve())
else:
    STATE_FILE = get_default_state_file()
PREFER_LOCAL_EMBEDDINGS = os.getenv("PREFER_LOCAL_EMBEDDINGS", "true").lower() == "true"
VOYAGE_API_KEY = os.getenv("VOYAGE_KEY")
MAX_CHUNK_SIZE = int(os.getenv("MAX_CHUNK_SIZE", "50"))  # Messages per chunk

# Initialize Qdrant client with timeout
client = QdrantClient(
    url=QDRANT_URL,
    timeout=30  # 30 second timeout for network operations
)

# Initialize embedding provider
embedding_provider = None
embedding_dimension = None

if PREFER_LOCAL_EMBEDDINGS or not VOYAGE_API_KEY:
    logger.info("Using local embeddings (fastembed)")
    from fastembed import TextEmbedding
    embedding_provider = TextEmbedding(model_name="sentence-transformers/all-MiniLM-L6-v2")
    embedding_dimension = 384
    collection_suffix = "local"
else:
    logger.info("Using Voyage AI embeddings")
    import voyageai
    embedding_provider = voyageai.Client(api_key=VOYAGE_API_KEY)
    embedding_dimension = 1024
    collection_suffix = "voyage"

def normalize_project_name(project_name: str) -> str:
    """Normalize project name for consistency."""
    # For compatibility with delta-metadata-update, just use the project name as-is
    # This ensures collection names match between import and delta update scripts
    return project_name

def get_collection_name(project_path: Path) -> str:
    """Generate collection name from project path."""
    normalized = normalize_project_name(project_path.name)
    name_hash = hashlib.md5(normalized.encode()).hexdigest()[:8]
    return f"conv_{name_hash}_{collection_suffix}"

def ensure_collection(collection_name: str):
    """Ensure collection exists with correct configuration."""
    collections = client.get_collections().collections
    if not any(c.name == collection_name for c in collections):
        logger.info(f"Creating collection: {collection_name}")
        client.create_collection(
            collection_name=collection_name,
            vectors_config=VectorParams(size=embedding_dimension, distance=Distance.COSINE)
        )

def generate_embeddings(texts: List[str]) -> List[List[float]]:
    """Generate embeddings for texts."""
    if PREFER_LOCAL_EMBEDDINGS or not VOYAGE_API_KEY:
        embeddings = list(embedding_provider.passage_embed(texts))
        return [emb.tolist() if hasattr(emb, 'tolist') else emb for emb in embeddings]
    else:
        response = embedding_provider.embed(texts, model="voyage-3")
        return response.embeddings

def process_and_upload_chunk(messages: List[Dict[str, Any]], chunk_index: int,
                            conversation_id: str, created_at: str,
                            metadata: Dict[str, Any], collection_name: str,
                            project_path: Path) -> int:
    """Process and immediately upload a single chunk."""
    if not messages:
        return 0
    
    # Extract text content
    texts = []
    for msg in messages:
        role = msg.get("role", "unknown")
        content = msg.get("content", "")
        if content:
            texts.append(f"{role.upper()}: {content}")
    
    if not texts:
        return 0
    
    chunk_text = "\n".join(texts)
    
    try:
        # Generate embedding
        embeddings = generate_embeddings([chunk_text])
        
        # Create point ID
        point_id = hashlib.md5(
            f"{conversation_id}_{chunk_index}".encode()
        ).hexdigest()[:16]
        
        # Create payload
        payload = {
            "text": chunk_text,
            "conversation_id": conversation_id,
            "chunk_index": chunk_index,
            "timestamp": created_at,
            "project": normalize_project_name(project_path.name),
            "start_role": messages[0].get("role", "unknown") if messages else "unknown",
            "message_count": len(messages)
        }
        
        # Add metadata
        if metadata:
            payload.update(metadata)
        
        # Create point
        point = PointStruct(
            id=int(point_id, 16) % (2**63),
            vector=embeddings[0],
            payload=payload
        )
        
        # Upload immediately (no wait for better throughput)
        client.upsert(
            collection_name=collection_name,
            points=[point],
            wait=False  # Don't wait for indexing to complete
        )
        
        return 1
        
    except Exception as e:
        logger.error(f"Error processing chunk {chunk_index}: {e}")
        return 0

def extract_metadata_single_pass(file_path: str) -> tuple[Dict[str, Any], str]:
    """Extract metadata in a single pass, return metadata and first timestamp."""
    metadata = {
        "files_analyzed": [],
        "files_edited": [],
        "tools_used": [],
        "concepts": []
    }
    
    first_timestamp = None
    
    try:
        with open(file_path, 'r', encoding='utf-8') as f:
            for line in f:
                if not line.strip():
                    continue
                    
                try:
                    data = json.loads(line)
                    
                    # Get timestamp from first valid entry
                    if first_timestamp is None and 'timestamp' in data:
                        first_timestamp = data.get('timestamp')
                    
                    # Extract tool usage from messages
                    if 'message' in data and data['message']:
                        msg = data['message']
                        if msg.get('content'):
                            content = msg['content']
                            if isinstance(content, list):
                                for item in content:
                                    if isinstance(item, dict) and item.get('type') == 'tool_use':
                                        tool_name = item.get('name', '')
                                        if tool_name and tool_name not in metadata['tools_used']:
                                            metadata['tools_used'].append(tool_name)
                                        
                                        # Extract file references
                                        if 'input' in item:
                                            input_data = item['input']
                                            if isinstance(input_data, dict):
                                                if 'file_path' in input_data:
                                                    file_ref = input_data['file_path']
                                                    if file_ref not in metadata['files_analyzed']:
                                                        metadata['files_analyzed'].append(file_ref)
                                                if 'path' in input_data:
                                                    file_ref = input_data['path']
                                                    if file_ref not in metadata['files_analyzed']:
                                                        metadata['files_analyzed'].append(file_ref)
                                        
                except json.JSONDecodeError:
                    continue
                except Exception:
                    continue
                    
    except Exception as e:
        logger.warning(f"Error extracting metadata: {e}")
    
    return metadata, first_timestamp or datetime.now().isoformat()

def stream_import_file(jsonl_file: Path, collection_name: str, project_path: Path) -> int:
    """Stream import a single JSONL file without loading it into memory."""
    logger.info(f"Streaming import of {jsonl_file.name}")
    
    # Extract metadata in first pass (lightweight)
    metadata, created_at = extract_metadata_single_pass(str(jsonl_file))
    
    # Stream messages and process in chunks
    chunk_buffer = []
    chunk_index = 0
    total_chunks = 0
    conversation_id = jsonl_file.stem
    
    try:
        with open(jsonl_file, 'r', encoding='utf-8') as f:
            for line_num, line in enumerate(f, 1):
                line = line.strip()
                if not line:
                    continue
                
                try:
                    data = json.loads(line)
                    
                    # Skip non-message lines
                    if data.get('type') == 'summary':
                        continue
                    
                    # Extract message if present
                    if 'message' in data and data['message']:
                        msg = data['message']
                        if msg.get('role') and msg.get('content'):
                            # Extract content
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
                                chunk_buffer.append({
                                    'role': msg['role'],
                                    'content': content
                                })
                                
                                # Process chunk when buffer reaches MAX_CHUNK_SIZE
                                if len(chunk_buffer) >= MAX_CHUNK_SIZE:
                                    chunks = process_and_upload_chunk(
                                        chunk_buffer, chunk_index, conversation_id,
                                        created_at, metadata, collection_name, project_path
                                    )
                                    total_chunks += chunks
                                    chunk_buffer = []
                                    chunk_index += 1
                                    
                                    # Force garbage collection after each chunk
                                    gc.collect()
                                    
                                    # Log progress
                                    if chunk_index % 10 == 0:
                                        logger.info(f"Processed {chunk_index} chunks from {jsonl_file.name}")
                                    
                except json.JSONDecodeError:
                    logger.debug(f"Skipping invalid JSON at line {line_num}")
                except Exception as e:
                    logger.debug(f"Error processing line {line_num}: {e}")
        
        # Process remaining messages
        if chunk_buffer:
            chunks = process_and_upload_chunk(
                chunk_buffer, chunk_index, conversation_id,
                created_at, metadata, collection_name, project_path
            )
            total_chunks += chunks
        
        logger.info(f"Imported {total_chunks} chunks from {jsonl_file.name}")
        return total_chunks
        
    except Exception as e:
        logger.error(f"Failed to import {jsonl_file}: {e}")
        return 0

def load_state() -> dict:
    """Load import state."""
    if os.path.exists(STATE_FILE):
        try:
            with open(STATE_FILE, 'r') as f:
                return json.load(f)
        except:
            pass
    return {"imported_files": {}}

def save_state(state: dict):
    """Save import state."""
    os.makedirs(os.path.dirname(STATE_FILE), exist_ok=True)
    with open(STATE_FILE, 'w') as f:
        json.dump(state, f, indent=2)

def should_import_file(file_path: Path, state: dict) -> bool:
    """Check if file should be imported."""
    file_str = str(file_path)
    if file_str in state.get("imported_files", {}):
        file_info = state["imported_files"][file_str]
        last_modified = file_path.stat().st_mtime
        if file_info.get("last_modified") == last_modified:
            logger.info(f"Skipping unchanged file: {file_path.name}")
            return False
    return True

def update_file_state(file_path: Path, state: dict, chunks: int):
    """Update state for imported file."""
    file_str = str(file_path)
    state["imported_files"][file_str] = {
        "imported_at": datetime.now().isoformat(),
        "last_modified": file_path.stat().st_mtime,
        "chunks": chunks
    }

def main():
    """Main import function."""
    # Load state
    state = load_state()
    logger.info(f"Loaded state with {len(state.get('imported_files', {}))} previously imported files")
    
    # Find all projects
    # Use LOGS_DIR env var, or fall back to Claude projects directory, then /logs for Docker
    logs_dir_env = os.getenv("LOGS_DIR")
    if logs_dir_env:
        logs_dir = Path(logs_dir_env)
    elif (Path.home() / ".claude" / "projects").exists():
        logs_dir = Path.home() / ".claude" / "projects"
    else:
        logs_dir = Path("/logs")  # Docker fallback
    
    if not logs_dir.exists():
        logger.error(f"Projects directory not found: {logs_dir}")
        sys.exit(1)
    
    project_dirs = [d for d in logs_dir.iterdir() if d.is_dir()]
    logger.info(f"Found {len(project_dirs)} projects to import")
    
    total_imported = 0
    
    for project_dir in project_dirs:
        # Get collection name
        collection_name = get_collection_name(project_dir)
        logger.info(f"Importing project: {project_dir.name} -> {collection_name}")
        
        # Ensure collection exists
        ensure_collection(collection_name)
        
        # Find JSONL files
        jsonl_files = sorted(project_dir.glob("*.jsonl"))
        
        # Limit files per cycle if specified
        max_files = int(os.getenv("MAX_FILES_PER_CYCLE", "1000"))
        jsonl_files = jsonl_files[:max_files]
        
        for jsonl_file in jsonl_files:
            if should_import_file(jsonl_file, state):
                chunks = stream_import_file(jsonl_file, collection_name, project_dir)
                if chunks > 0:
                    update_file_state(jsonl_file, state, chunks)
                    save_state(state)
                    total_imported += 1
                    
                    # Force GC after each file
                    gc.collect()
    
    logger.info(f"Import complete: processed {total_imported} files")

if __name__ == "__main__":
    main()