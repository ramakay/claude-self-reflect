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
import ast
import re
from pathlib import Path
from datetime import datetime
from typing import List, Dict, Any, Optional, Set
import logging

# Add the scripts directory to the Python path for utils import
scripts_dir = Path(__file__).parent
sys.path.insert(0, str(scripts_dir))

from qdrant_client import QdrantClient
from qdrant_client.models import PointStruct, Distance, VectorParams

# Import the correct normalize_project_name from utils
try:
    from utils import normalize_project_name
except ImportError as e:
    logging.error(f"Failed to import normalize_project_name from utils: {e}")
    sys.exit(1)

# Set up logging
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

# Environment variables
QDRANT_URL = os.getenv("QDRANT_URL", "http://localhost:6333")

# Constants for metadata limits
MAX_CONCEPTS = 10
MAX_AST_ELEMENTS = 30
MAX_CODE_BLOCKS = 5
MAX_ELEMENTS_PER_BLOCK = 10

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
    # Using the same model as official Qdrant MCP server
    embedding_provider = TextEmbedding(model_name="sentence-transformers/all-MiniLM-L6-v2")
    embedding_dimension = 384
    collection_suffix = "local"
    logger.info("Using fastembed model: sentence-transformers/all-MiniLM-L6-v2")
else:
    logger.info("Using Voyage AI embeddings")
    import voyageai
    embedding_provider = voyageai.Client(api_key=VOYAGE_API_KEY)
    embedding_dimension = 1024
    collection_suffix = "voyage"

def get_collection_name(project_path: Path) -> str:
    """Generate collection name from project path."""
    normalized = normalize_project_name(str(project_path))
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
                            project_path: Path, total_messages: int) -> int:
    """Process and immediately upload a single chunk."""
    if not messages:
        return 0
    
    # Extract text content and message indices
    texts = []
    message_indices = []
    for msg in messages:
        role = msg.get("role", "unknown")
        content = msg.get("content", "")
        if content:
            texts.append(f"{role.upper()}: {content}")
            # Fix: Check for None instead of truthiness to include 0 values
            idx = msg.get("message_index")
            if idx is not None:
                message_indices.append(idx)
    
    if not texts:
        return 0
    
    chunk_text = "\n".join(texts)
    
    try:
        # Generate embedding
        embeddings = generate_embeddings([chunk_text])
        
        # Sanity check embeddings
        if not embeddings or not embeddings[0]:
            logger.error(f"Empty embedding generated for chunk {chunk_index}")
            return 0
        
        embedding = embeddings[0]
        
        # Check for degenerate embeddings (all values identical)
        if len(set(embedding)) == 1:
            logger.error(f"Degenerate embedding detected (all values identical): {embedding[0]}")
            return 0
        
        # Check variance is above threshold
        import statistics
        variance = statistics.variance(embedding)
        if variance < 1e-6:
            logger.warning(f"Low variance embedding detected: {variance}")
        
        # Validate dimension
        if len(embedding) != embedding_dimension:
            logger.error(f"Embedding dimension mismatch: expected {embedding_dimension}, got {len(embedding)}")
            return 0
        
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
            "project": normalize_project_name(str(project_path)),
            "start_role": messages[0].get("role", "unknown") if messages else "unknown",
            "message_count": len(messages),
            "total_messages": total_messages,
            "message_index": message_indices[0] if message_indices else 0,
            "message_indices": message_indices  # Store all indices in this chunk
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

def extract_ast_elements(code_text: str) -> Set[str]:
    """Extract function and class names from code using AST parsing."""
    elements = set()
    
    # Try to parse as Python code
    try:
        tree = ast.parse(code_text)
        for node in ast.walk(tree):
            if isinstance(node, ast.FunctionDef):
                elements.add(f"func:{node.name}")
            elif isinstance(node, ast.AsyncFunctionDef):
                elements.add(f"func:{node.name}")
            elif isinstance(node, ast.ClassDef):
                elements.add(f"class:{node.name}")
    except SyntaxError:
        # Python regex fallback for partial fragments
        for m in re.finditer(r'^\s*def\s+([A-Za-z_]\w*)\s*\(', code_text, re.MULTILINE):
            elements.add(f"func:{m.group(1)}")
        for m in re.finditer(r'^\s*async\s+def\s+([A-Za-z_]\w*)\s*\(', code_text, re.MULTILINE):
            elements.add(f"func:{m.group(1)}")
        for m in re.finditer(r'^\s*class\s+([A-Za-z_]\w*)\s*[:\(]', code_text, re.MULTILINE):
            elements.add(f"class:{m.group(1)}")
    except Exception as e:
        logger.debug(f"Unexpected error parsing AST: {e}")
        
    # Try regex patterns for other languages
    # JavaScript/TypeScript functions
    js_func_pattern = r'(?:function|const|let|var)\s+(\w+)\s*(?:=\s*)?(?:\([^)]*\)|\s*=>)'
    for match in re.finditer(js_func_pattern, code_text):
        elements.add(f"func:{match.group(1)}")
    
    # Class definitions (multiple languages)
    class_pattern = r'(?:class|interface|struct)\s+(\w+)'
    for match in re.finditer(class_pattern, code_text):
        elements.add(f"class:{match.group(1)}")
    
    return elements

def extract_concepts(text: str) -> List[str]:
    """Extract development concepts from text."""
    concepts = []
    concept_patterns = {
        'docker': r'\b(?:docker|container|compose|dockerfile)\b',
        'testing': r'\b(?:test|testing|unittest|pytest|jest)\b',
        'database': r'\b(?:database|sql|postgres|mysql|mongodb|qdrant)\b',
        'api': r'\b(?:api|rest|graphql|endpoint)\b',
        'security': r'\b(?:security|auth|authentication|encryption)\b',
        'performance': r'\b(?:performance|optimization|cache|speed)\b',
        'debugging': r'\b(?:debug|debugging|error|bug|trace)\b',
        'deployment': r'\b(?:deploy|deployment|ci\/cd|production)\b',
        'git': r'\b(?:git|commit|branch|merge|pull request)\b',
        'mcp': r'\b(?:mcp|claude-self-reflect|claude code)\b',
        'embeddings': r'\b(?:embedding|vector|semantic|similarity)\b',
    }
    
    text_lower = text.lower()
    for concept, pattern in concept_patterns.items():
        if re.search(pattern, text_lower, re.IGNORECASE):
            if concept not in concepts:
                concepts.append(concept)
    
    return concepts[:MAX_CONCEPTS]

def extract_metadata_single_pass(file_path: str) -> tuple[Dict[str, Any], str, int]:
    """Extract metadata in a single pass, return metadata, first timestamp, and message count."""
    metadata = {
        "files_analyzed": [],
        "files_edited": [],
        "tools_used": [],
        "concepts": [],
        "ast_elements": [],
        "has_code_blocks": False,
        "total_messages": 0
    }
    
    first_timestamp = None
    message_count = 0
    all_text = []
    
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
                    
                    # Count messages
                    if 'message' in data and data['message']:
                        msg = data['message']
                        if msg.get('role') in ['user', 'assistant']:
                            message_count += 1
                        
                        if msg.get('content'):
                            content = msg['content']
                            text_content = ""
                            
                            if isinstance(content, list):
                                for item in content:
                                    if isinstance(item, dict):
                                        if item.get('type') == 'text':
                                            text_content += item.get('text', '')
                                            # Check for code blocks
                                            if '```' in item.get('text', ''):
                                                metadata['has_code_blocks'] = True
                                                # Extract code for AST analysis with bounds checking
                                                if len(metadata['ast_elements']) < 30:
                                                    # Fix: More permissive regex to handle various fence formats
                                                    code_blocks = re.findall(r'```[^\n]*\n?(.*?)```', item.get('text', ''), re.DOTALL)
                                                    for code_block in code_blocks[:5]:  # Limit to 5 blocks
                                                        if len(metadata['ast_elements']) >= 30:
                                                            break
                                                        ast_elems = extract_ast_elements(code_block)
                                                        for elem in list(ast_elems)[:10]:  # Limit elements per block
                                                            if elem not in metadata['ast_elements'] and len(metadata['ast_elements']) < 30:
                                                                metadata['ast_elements'].append(elem)
                                        
                                        elif item.get('type') == 'tool_use':
                                            tool_name = item.get('name', '')
                                            if tool_name and tool_name not in metadata['tools_used']:
                                                metadata['tools_used'].append(tool_name)
                                            
                                            # Extract file references
                                            if 'input' in item:
                                                input_data = item['input']
                                                if isinstance(input_data, dict):
                                                    # Determine if it's an edit tool
                                                    is_edit = tool_name in ['Edit', 'Write', 'MultiEdit', 'NotebookEdit']
                                                    
                                                    if 'file_path' in input_data:
                                                        file_ref = input_data['file_path']
                                                        if is_edit:
                                                            if file_ref not in metadata['files_edited']:
                                                                metadata['files_edited'].append(file_ref)
                                                        else:
                                                            if file_ref not in metadata['files_analyzed']:
                                                                metadata['files_analyzed'].append(file_ref)
                                                    
                                                    if 'path' in input_data:
                                                        file_ref = input_data['path']
                                                        if file_ref not in metadata['files_analyzed']:
                                                            metadata['files_analyzed'].append(file_ref)
                                    elif isinstance(item, str):
                                        text_content += item
                            elif isinstance(content, str):
                                text_content = content
                            
                            # Collect text for concept extraction
                            if text_content:
                                all_text.append(text_content[:1000])  # Limit text per message
                                        
                except json.JSONDecodeError:
                    continue
                except Exception:
                    continue
    
    except Exception as e:
        logger.warning(f"Error extracting metadata: {e}")
    
    # Extract concepts from collected text
    if all_text:
        combined_text = ' '.join(all_text[:50])  # Limit to first 50 messages
        metadata['concepts'] = extract_concepts(combined_text)
    
    # Set total messages
    metadata['total_messages'] = message_count
    
    # Limit arrays
    metadata['files_analyzed'] = metadata['files_analyzed'][:20]
    metadata['files_edited'] = metadata['files_edited'][:20]
    metadata['tools_used'] = metadata['tools_used'][:15]
    metadata['ast_elements'] = metadata['ast_elements'][:30]
    
    return metadata, first_timestamp or datetime.now().isoformat(), message_count

def stream_import_file(jsonl_file: Path, collection_name: str, project_path: Path) -> int:
    """Stream import a single JSONL file without loading it into memory."""
    logger.info(f"Streaming import of {jsonl_file.name}")
    
    # Extract metadata in first pass (lightweight)
    metadata, created_at, total_messages = extract_metadata_single_pass(str(jsonl_file))
    
    # Stream messages and process in chunks
    chunk_buffer = []
    chunk_index = 0
    total_chunks = 0
    conversation_id = jsonl_file.stem
    current_message_index = 0
    
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
                                # Track message index for user/assistant messages
                                if msg['role'] in ['user', 'assistant']:
                                    current_message_index += 1
                                    message_idx = current_message_index
                                else:
                                    message_idx = 0
                                
                                chunk_buffer.append({
                                    'role': msg['role'],
                                    'content': content,
                                    'message_index': message_idx
                                })
                                
                                # Process chunk when buffer reaches MAX_CHUNK_SIZE
                                if len(chunk_buffer) >= MAX_CHUNK_SIZE:
                                    chunks = process_and_upload_chunk(
                                        chunk_buffer, chunk_index, conversation_id,
                                        created_at, metadata, collection_name, project_path, total_messages
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
                created_at, metadata, collection_name, project_path, total_messages
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
    """Save import state with atomic write."""
    # Fix: Handle case where STATE_FILE has no directory component
    state_dir = os.path.dirname(STATE_FILE)
    if state_dir:
        os.makedirs(state_dir, exist_ok=True)
    
    # Use atomic write to prevent corruption during crashes
    temp_file = f"{STATE_FILE}.tmp"
    with open(temp_file, 'w') as f:
        json.dump(state, f, indent=2)
    
    # Atomic rename (on POSIX systems)
    os.replace(temp_file, STATE_FILE)

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