#!/usr/bin/env python3
"""
Unified import script that supports both local and Voyage AI embeddings.
"""

import os
import sys
import json
import glob
import hashlib
import gc
import re
from datetime import datetime
from typing import List, Dict, Any, Set
import logging
from pathlib import Path

# Add the mcp-server/src directory to the Python path
sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), 'mcp-server', 'src'))
from utils import normalize_project_name

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
# Default to project config directory for state file
default_state_file = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "config", "imported-files.json")
STATE_FILE = os.getenv("STATE_FILE", default_state_file)
BATCH_SIZE = int(os.getenv("BATCH_SIZE", "10"))  # Reduced from 100 to prevent OOM
PREFER_LOCAL_EMBEDDINGS = os.getenv("PREFER_LOCAL_EMBEDDINGS", "false").lower() == "true"
VOYAGE_API_KEY = os.getenv("VOYAGE_KEY")
CURRENT_METADATA_VERSION = 2  # Version 2: Added tool output extraction

# Token limit configuration for Voyage AI
MAX_TOKENS_PER_BATCH = int(os.getenv("MAX_TOKENS_PER_BATCH", "100000"))  # Safe limit (120k - 20k buffer)
if MAX_TOKENS_PER_BATCH > 120000 or MAX_TOKENS_PER_BATCH < 1000:
    logger.warning(f"MAX_TOKENS_PER_BATCH={MAX_TOKENS_PER_BATCH} outside safe range [1000, 120000], using 100000")
    MAX_TOKENS_PER_BATCH = 100000

TOKEN_ESTIMATION_RATIO = int(os.getenv("TOKEN_ESTIMATION_RATIO", "3"))  # chars per token estimate
if TOKEN_ESTIMATION_RATIO < 2 or TOKEN_ESTIMATION_RATIO > 10:
    logger.warning(f"TOKEN_ESTIMATION_RATIO={TOKEN_ESTIMATION_RATIO} outside normal range [2, 10], using 3")
    TOKEN_ESTIMATION_RATIO = 3

USE_TOKEN_AWARE_BATCHING = os.getenv("USE_TOKEN_AWARE_BATCHING", "true").lower() == "true"
MAX_RECURSION_DEPTH = 10  # Maximum depth for recursive chunk splitting

# Set up logging
logging.basicConfig(level=logging.INFO, format='%(asctime)s - %(levelname)s - %(message)s')
logger = logging.getLogger(__name__)

# ============= Metadata Extraction Functions =============

def normalize_path_for_metadata(path: str) -> str:
    """Normalize file paths for consistency in metadata."""
    if not path:
        return ""
    
    # Remove common prefixes
    path = path.replace("/Users/", "~/")
    path = path.replace("\\Users\\", "~\\")
    
    # Convert to forward slashes
    path = path.replace("\\", "/")
    
    # Remove duplicate slashes
    path = re.sub(r'/+', '/', path)
    
    return path

def extract_concepts(text: str, tool_usage: Dict[str, Any]) -> Set[str]:
    """Extract high-level concepts from conversation and tool usage."""
    concepts = set()
    
    # Common development concepts with patterns
    concept_patterns = {
        'security': r'(security|vulnerability|CVE|injection|sanitize|escape|auth|token|JWT)',
        'performance': r'(performance|optimization|speed|memory|efficient|benchmark|latency)',
        'testing': r'(test|pytest|unittest|coverage|TDD|spec|assert)',
        'docker': r'(docker|container|compose|dockerfile|kubernetes|k8s)',
        'api': r'(API|REST|GraphQL|endpoint|webhook|http|request)',
        'database': r'(database|SQL|query|migration|schema|postgres|mysql|mongodb|qdrant)',
        'authentication': r'(auth|login|token|JWT|session|oauth|permission)',
        'debugging': r'(debug|error|exception|traceback|log|stack|trace)',
        'refactoring': r'(refactor|cleanup|improve|restructure|optimize|technical debt)',
        'deployment': r'(deploy|CI/CD|release|production|staging|rollout)',
        'git': r'(git|commit|branch|merge|pull request|PR|rebase)',
        'architecture': r'(architecture|design|pattern|structure|component|module)',
        'mcp': r'(MCP|claude-self-reflect|tool|agent|claude code)',
        'embeddings': r'(embedding|vector|semantic|similarity|fastembed|voyage)',
        'search': r'(search|query|find|filter|match|relevance)'
    }
    
    # Check text content (limit to first 10000 chars for performance)
    combined_text = text[:10000].lower() if text else ""
    for concept, pattern in concept_patterns.items():
        if re.search(pattern, combined_text, re.IGNORECASE):
            concepts.add(concept)
    
    # Check tool usage patterns
    if tool_usage.get('grep_searches'):
        concepts.add('search')
    if tool_usage.get('files_edited') or tool_usage.get('files_created'):
        concepts.add('development')
    if any('test' in str(f).lower() for f in tool_usage.get('files_read', [])):
        concepts.add('testing')
    if any('docker' in str(cmd).lower() for cmd in tool_usage.get('bash_commands', [])):
        concepts.add('docker')
    
    return concepts

def extract_files_from_git_output(output_text: str) -> List[str]:
    """Extract file paths from git command outputs (diff, show, status, etc)."""
    files = set()
    
    # Patterns for different git output formats
    patterns = [
        r'diff --git a/(.*?) b/',  # git diff format
        r'^\+\+\+ b/(.+)$',  # diff new file
        r'^--- a/(.+)$',  # diff old file
        r'^modified:\s+(.+)$',  # git status
        r'^deleted:\s+(.+)$',  # git status
        r'^new file:\s+(.+)$',  # git status
        r'^renamed:\s+(.+) -> (.+)$',  # git status (captures both)
    ]
    
    for pattern in patterns:
        matches = re.findall(pattern, output_text, re.MULTILINE)
        for match in matches:
            if isinstance(match, tuple):
                # Handle renamed files (captures both old and new)
                for f in match:
                    if f:
                        files.add(normalize_path_for_metadata(f))
            else:
                files.add(normalize_path_for_metadata(match))
    
    return list(files)[:20]  # Limit to 20 files

def extract_tool_data_from_message(tool_use: Dict[str, Any], usage_dict: Dict[str, Any], tool_output: str = None):
    """Extract tool usage data from a tool_use object in a message, including outputs."""
    tool_name = tool_use.get('name', '')
    inputs = tool_use.get('input', {})
    
    # Track tool in summary
    usage_dict['tools_summary'][tool_name] = usage_dict['tools_summary'].get(tool_name, 0) + 1
    
    # Handle Read tool
    if tool_name == 'Read':
        file_path = inputs.get('file_path')
        if file_path:
            normalized = normalize_path_for_metadata(file_path)
            if normalized not in usage_dict['files_read']:
                usage_dict['files_read'].append(normalized)
    
    # Handle Edit and MultiEdit tools
    elif tool_name in ['Edit', 'MultiEdit']:
        path = inputs.get('file_path')
        if path:
            normalized = normalize_path_for_metadata(path)
            if normalized not in usage_dict['files_edited']:
                usage_dict['files_edited'].append(normalized)
    
    # Handle Write tool
    elif tool_name == 'Write':
        path = inputs.get('file_path')
        if path:
            normalized = normalize_path_for_metadata(path)
            if normalized not in usage_dict['files_created']:
                usage_dict['files_created'].append(normalized)
    
    # Handle Grep tool
    elif tool_name == 'Grep':
        pattern = inputs.get('pattern')
        if pattern and len(usage_dict['grep_searches']) < 10:  # Limit
            usage_dict['grep_searches'].append(pattern[:100])  # Truncate long patterns
    
    # Handle Bash tool - Extract both command and output
    elif tool_name == 'Bash':
        command = inputs.get('command')
        if command and len(usage_dict['bash_commands']) < 10:
            usage_dict['bash_commands'].append(command[:200])  # Truncate
            
            # Process tool output for git commands
            if tool_output and any(cmd in command for cmd in ['git diff', 'git show', 'git status']):
                git_files = extract_files_from_git_output(tool_output)
                for file_path in git_files:
                    if file_path not in usage_dict['git_file_changes']:
                        usage_dict['git_file_changes'].append(file_path)
    
    # Store tool output preview (for any tool)
    if tool_output and len(usage_dict['tool_outputs']) < 15:
        usage_dict['tool_outputs'].append({
            'tool': tool_name,
            'command': inputs.get('command', inputs.get('pattern', ''))[:100],
            'output_preview': tool_output[:500],  # First 500 chars
            'output_length': len(tool_output)
        })

def extract_metadata_from_jsonl(file_path: str) -> Dict[str, Any]:
    """Extract metadata from a JSONL conversation file."""
    tool_usage = {
        "files_read": [],
        "files_edited": [],
        "files_created": [],
        "grep_searches": [],
        "bash_commands": [],
        "tools_summary": {},
        "git_file_changes": [],  # NEW: Files from git outputs
        "tool_outputs": []  # NEW: Tool output previews
    }
    
    conversation_text = ""
    tool_outputs = {}  # Map tool_use_id to output text
    
    try:
        # First pass: collect tool outputs
        with open(file_path, 'r', encoding='utf-8') as f:
            for line in f:
                if line.strip():
                    try:
                        data = json.loads(line)
                        if 'message' in data and data['message']:
                            msg = data['message']
                            if msg.get('content') and isinstance(msg['content'], list):
                                for item in msg['content']:
                                    if isinstance(item, dict) and item.get('type') == 'tool_result':
                                        # Capture tool output
                                        tool_id = item.get('tool_use_id')
                                        output_content = item.get('content', '')
                                        if tool_id and output_content:
                                            tool_outputs[tool_id] = output_content
                        # Also check for toolUseResult in data
                        if 'toolUseResult' in data:
                            result = data['toolUseResult']
                            if isinstance(result, dict):
                                tool_outputs['last_result'] = json.dumps(result)[:1000]
                    except:
                        continue
        
        # Second pass: extract tool uses and text with outputs available
        with open(file_path, 'r', encoding='utf-8') as f:
            for line in f:
                if line.strip():
                    try:
                        data = json.loads(line)
                        if 'message' in data and data['message']:
                            msg = data['message']
                            # Extract text
                            if msg.get('content'):
                                if isinstance(msg['content'], str):
                                    conversation_text += msg['content'] + "\n"
                                elif isinstance(msg['content'], list):
                                    for item in msg['content']:
                                        if isinstance(item, dict):
                                            if item.get('type') == 'text' and item.get('text'):
                                                conversation_text += item['text'] + "\n"
                                            elif item.get('type') == 'tool_use':
                                                # Process tool use with output now available
                                                tool_id = item.get('id', '')
                                                output = tool_outputs.get(tool_id, '')
                                                extract_tool_data_from_message(item, tool_usage, output)
                    except:
                        continue
    except Exception as e:
        logger.warning(f"Error extracting metadata from {file_path}: {e}")
    
    # Extract concepts from text
    concepts = extract_concepts(conversation_text, tool_usage)
    
    # Build metadata
    metadata = {
        "files_analyzed": tool_usage['files_read'][:20],  # Limit to 20
        "files_edited": tool_usage['files_edited'][:10],  # Limit to 10
        "files_created": tool_usage['files_created'][:10],
        "tools_used": list(tool_usage['tools_summary'].keys())[:20],
        "tool_summary": dict(list(tool_usage['tools_summary'].items())[:10]),
        "concepts": list(concepts)[:15],  # Limit to 15
        "search_patterns": tool_usage['grep_searches'][:10],
        "git_file_changes": tool_usage['git_file_changes'][:20],  # NEW: Git file changes
        "tool_outputs": tool_usage['tool_outputs'][:15],  # NEW: Tool output previews
        "analysis_only": len(tool_usage['files_edited']) == 0 and len(tool_usage['files_created']) == 0,
        "has_file_metadata": True,
        "metadata_version": CURRENT_METADATA_VERSION,
        "metadata_extracted_at": datetime.now().isoformat()
    }
    
    return metadata

# ============= End Metadata Extraction Functions =============

# State management functions
def load_state():
    """Load the import state from file."""
    if os.path.exists(STATE_FILE):
        try:
            with open(STATE_FILE, 'r') as f:
                state = json.load(f)
                # Ensure the expected structure exists
                if "imported_files" not in state:
                    state["imported_files"] = {}
                return state
        except Exception as e:
            logger.warning(f"Failed to load state file: {e}")
    return {"imported_files": {}}

def save_state(state):
    """Save the import state to file."""
    try:
        # Ensure directory exists
        os.makedirs(os.path.dirname(STATE_FILE), exist_ok=True)
        # Write atomically by using a temp file
        temp_file = STATE_FILE + ".tmp"
        with open(temp_file, 'w') as f:
            json.dump(state, f, indent=2)
        os.replace(temp_file, STATE_FILE)
        logger.debug(f"Saved state with {len(state['imported_files'])} files")
    except Exception as e:
        logger.error(f"Failed to save state file: {e}")

def should_import_file(file_path, state):
    """Check if a file should be imported based on modification time."""
    str_path = str(file_path)
    file_mtime = os.path.getmtime(file_path)
    
    if str_path in state["imported_files"]:
        file_state = state["imported_files"][str_path]
        
        # Handle both old string format and new dict format
        if isinstance(file_state, str):
            # Old format (just timestamp string) - treat as needs reimport
            logger.info(f"Found old format state for {file_path.name}, will reimport")
            return True
        else:
            # New format with dictionary
            last_imported = file_state.get("last_imported", 0)
            last_modified = file_state.get("last_modified", 0)
            
            # Skip if file hasn't been modified since last import
            if file_mtime <= last_modified and last_imported > 0:
                logger.info(f"Skipping unchanged file: {file_path.name}")
                return False
    
    return True

def update_file_state(file_path, state, chunks_imported):
    """Update the state for an imported file."""
    str_path = str(file_path)
    state["imported_files"][str_path] = {
        "last_modified": os.path.getmtime(file_path),
        "last_imported": datetime.now().timestamp(),
        "chunks_imported": chunks_imported
    }

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

def estimate_tokens(text: str) -> int:
    """Estimate token count for text with content-aware heuristics.
    Base estimate uses TOKEN_ESTIMATION_RATIO, adjusted for content type.
    """
    # Base estimate
    base_tokens = len(text) // TOKEN_ESTIMATION_RATIO
    
    # Adjust for code/JSON content (typically more tokens per char)
    # Count indicators of structured content
    structure_indicators = text.count('{') + text.count('[') + text.count('```')
    if structure_indicators > 10:  # Likely JSON/code
        base_tokens = int(base_tokens * 1.3)
    
    # Add 10% safety margin
    return int(base_tokens * 1.1)

def extract_message_content(msg: Dict[str, Any]) -> str:
    """Extract text content from a message."""
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
    
    return content

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

def split_large_chunk(chunk: Dict[str, Any], max_tokens: int, depth: int = 0) -> List[Dict[str, Any]]:
    """Split a large chunk into smaller pieces that fit token limit."""
    # Check recursion depth to prevent stack overflow
    if depth >= MAX_RECURSION_DEPTH:
        logger.error(f"Max recursion depth {MAX_RECURSION_DEPTH} reached while splitting chunk")
        # Force truncate as last resort
        max_chars = max_tokens * TOKEN_ESTIMATION_RATIO
        chunk["text"] = chunk["text"][:max_chars] + "\n[TRUNCATED - MAX DEPTH REACHED]"
        chunk["was_truncated"] = True
        return [chunk]
    
    text = chunk["text"]
    messages = chunk["messages"]
    
    # First, check if we can split by messages
    if len(messages) > 1:
        # Try splitting messages into smaller groups
        mid = len(messages) // 2
        chunk1_messages = messages[:mid]
        chunk2_messages = messages[mid:]
        
        # Recreate text for each split
        texts1 = []
        texts2 = []
        
        for msg in chunk1_messages:
            role = msg.get("role", "unknown")
            content = extract_message_content(msg)
            if content:
                texts1.append(f"{role.upper()}: {content}")
        
        for msg in chunk2_messages:
            role = msg.get("role", "unknown")
            content = extract_message_content(msg)
            if content:
                texts2.append(f"{role.upper()}: {content}")
        
        split_chunks = []
        if texts1:
            split_chunks.append({
                "text": "\n".join(texts1),
                "messages": chunk1_messages,
                "chunk_index": f"{chunk['chunk_index']}_a",
                "start_role": chunk["start_role"]
            })
        if texts2:
            split_chunks.append({
                "text": "\n".join(texts2),
                "messages": chunk2_messages,
                "chunk_index": f"{chunk['chunk_index']}_b",
                "start_role": chunk2_messages[0].get("role", "unknown") if chunk2_messages else "unknown"
            })
        
        # Recursively split if still too large
        result = []
        for split_chunk in split_chunks:
            if estimate_tokens(split_chunk["text"]) > max_tokens:
                result.extend(split_large_chunk(split_chunk, max_tokens, depth + 1))
            else:
                result.append(split_chunk)
        return result
    else:
        # Single message too large - truncate with warning
        max_chars = max_tokens * TOKEN_ESTIMATION_RATIO
        if len(text) > max_chars:
            truncated_size = len(text) - max_chars
            logger.warning(f"Single message exceeds token limit, truncating {truncated_size} chars from {len(text)} total")
            chunk["text"] = text[:max_chars] + f"\n[TRUNCATED {truncated_size} CHARS]"
            chunk["was_truncated"] = True
            chunk["original_size"] = len(text)
        return [chunk]

def create_token_aware_batches(chunks: List[Dict[str, Any]], max_tokens: int = MAX_TOKENS_PER_BATCH) -> List[List[Dict[str, Any]]]:
    """Create batches that respect token limits."""
    if not USE_TOKEN_AWARE_BATCHING:
        # Fall back to old batching method
        batches = []
        for i in range(0, len(chunks), BATCH_SIZE):
            batches.append(chunks[i:i + BATCH_SIZE])
        return batches
    
    batches = []
    current_batch = []
    current_tokens = 0
    
    for chunk in chunks:
        chunk_tokens = estimate_tokens(chunk["text"])
        
        # If single chunk exceeds limit, split it
        if chunk_tokens > max_tokens:
            logger.warning(f"Chunk with {chunk_tokens} estimated tokens exceeds limit of {max_tokens}, splitting...")
            split_chunks = split_large_chunk(chunk, max_tokens)
            for split_chunk in split_chunks:
                split_tokens = estimate_tokens(split_chunk["text"])
                if split_tokens > max_tokens:
                    logger.error(f"Split chunk still exceeds limit: {split_tokens} tokens")
                batches.append([split_chunk])
        # If adding chunk would exceed limit, start new batch
        elif current_tokens + chunk_tokens > max_tokens:
            if current_batch:
                batches.append(current_batch)
            current_batch = [chunk]
            current_tokens = chunk_tokens
        else:
            current_batch.append(chunk)
            current_tokens += chunk_tokens
    
    if current_batch:
        batches.append(current_batch)
    
    # Log batch statistics
    if batches:
        batch_sizes = [len(batch) for batch in batches]
        batch_tokens = [sum(estimate_tokens(chunk["text"]) for chunk in batch) for batch in batches]
        logger.debug(f"Created {len(batches)} batches, chunk counts: min={min(batch_sizes)}, max={max(batch_sizes)}, "
                    f"estimated tokens: min={min(batch_tokens)}, max={max(batch_tokens)}, avg={sum(batch_tokens)//len(batches)}")
    
    return batches

def import_project(project_path: Path, collection_name: str, state: dict) -> int:
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
        # Check if file should be imported
        if not should_import_file(jsonl_file, state):
            continue
            
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
            
            # Extract tool usage metadata from the file
            metadata = extract_metadata_from_jsonl(str(jsonl_file))
            
            # Chunk the conversation
            chunks = chunk_conversation(messages)
            
            if not chunks:
                continue
            
            # Process in batches (token-aware if enabled)
            token_aware_batches = create_token_aware_batches(chunks)
            
            for batch_idx, batch in enumerate(token_aware_batches):
                texts = [chunk["text"] for chunk in batch]
                
                # Log batch info for debugging
                if USE_TOKEN_AWARE_BATCHING:
                    total_tokens = sum(estimate_tokens(text) for text in texts)
                    logger.debug(f"Batch {batch_idx + 1}/{len(token_aware_batches)}: {len(texts)} chunks, ~{total_tokens} estimated tokens")
                
                # Generate embeddings
                embeddings = generate_embeddings(texts)
                
                # Create points
                points = []
                for chunk, embedding in zip(batch, embeddings):
                    point_id = hashlib.md5(
                        f"{conversation_id}_{chunk['chunk_index']}".encode()
                    ).hexdigest()[:16]
                    
                    # Combine basic payload with metadata
                    payload = {
                        "text": chunk["text"],
                        "conversation_id": conversation_id,
                        "chunk_index": chunk["chunk_index"],
                        "timestamp": created_at,
                        "project": normalize_project_name(project_path.name),
                        "start_role": chunk["start_role"]
                    }
                    # Add metadata fields
                    payload.update(metadata)
                    
                    points.append(PointStruct(
                        id=int(point_id, 16) % (2**63),  # Convert to valid integer ID
                        vector=embedding,
                        payload=payload
                    ))
                
                # Upload to Qdrant
                client.upsert(
                    collection_name=collection_name,
                    points=points
                )
                
                total_chunks += len(points)
            
            file_chunks = len(chunks)
            logger.info(f"Imported {file_chunks} chunks from {jsonl_file.name}")
            
            # Update state for this file
            update_file_state(jsonl_file, state, file_chunks)
            
            # Save state after each file to prevent loss on OOM
            save_state(state)
            
            # Force garbage collection to free memory
            gc.collect()
            
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
    
    # Load existing state
    state = load_state()
    logger.info(f"Loaded state with {len(state['imported_files'])} previously imported files")
    
    # Find all project directories
    project_dirs = [d for d in logs_path.iterdir() if d.is_dir()]
    
    if not project_dirs:
        logger.warning("No project directories found")
        return
    
    logger.info(f"Found {len(project_dirs)} projects to import")
    
    # Import each project
    total_imported = 0
    for project_dir in project_dirs:
        # Create collection name from normalized project name
        normalized_name = normalize_project_name(project_dir.name)
        collection_name = f"conv_{hashlib.md5(normalized_name.encode()).hexdigest()[:8]}{collection_suffix}"
        
        logger.info(f"Importing project: {project_dir.name} (normalized: {normalized_name}) -> {collection_name}")
        chunks = import_project(project_dir, collection_name, state)
        total_imported += chunks
        logger.info(f"Imported {chunks} chunks from {project_dir.name}")
        
        # Save state after each project to avoid losing progress
        save_state(state)
    
    # Final save (redundant but ensures state is saved)
    save_state(state)
    
    logger.info(f"Import complete! Total chunks imported: {total_imported}")

if __name__ == "__main__":
    main()