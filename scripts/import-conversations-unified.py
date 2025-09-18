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
import fcntl
import time
import argparse
from pathlib import Path
from datetime import datetime, timezone
from typing import List, Dict, Any, Optional, Set
import logging

# Load .env file if it exists
try:
    from dotenv import load_dotenv
    # Load from project root
    env_path = Path(__file__).parent.parent / '.env'
    if env_path.exists():
        load_dotenv(env_path)
        print(f"Loaded .env from {env_path}")
except ImportError:
    pass  # dotenv not available, use system environment

# Add the scripts directory to the Python path for utils import
scripts_dir = Path(__file__).parent
sys.path.insert(0, str(scripts_dir))

# Import UnifiedStateManager
from unified_state_manager import UnifiedStateManager

from qdrant_client import QdrantClient
from qdrant_client.models import PointStruct, Distance, VectorParams

# Import normalize_project_name from shared module
# Add parent directory to path to import shared module
sys.path.insert(0, str(Path(__file__).parent.parent))
try:
    from shared.normalization import normalize_project_name
except ImportError as e:
    logging.error(f"Failed to import normalize_project_name from shared module: {e}")
    # Fall back to local utils if shared module not found
    try:
        from utils import normalize_project_name
        logging.warning("Using legacy utils.normalize_project_name - consider updating")
    except ImportError:
        logging.error("Could not import normalize_project_name from any source")
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
MAX_FILES_ANALYZED = 20
MAX_FILES_EDITED = 20
MAX_TOOLS_USED = 15
MAX_CONCEPT_MESSAGES = 50

# Initialize UnifiedStateManager
# Support legacy STATE_FILE environment variable
env_state = os.getenv("STATE_FILE")
if env_state:
    from pathlib import Path
    state_file_path = Path(env_state).expanduser().resolve()
    state_manager = UnifiedStateManager(state_file_path)
else:
    state_manager = UnifiedStateManager()  # Uses default location
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
    # Use the global embedding_provider which gets updated by command-line args
    if PREFER_LOCAL_EMBEDDINGS:
        # FastEmbed uses 'embed' method, not 'passage_embed'
        # Try 'embed' first, fall back to 'passage_embed' for compatibility
        if hasattr(embedding_provider, 'embed'):
            embeddings = list(embedding_provider.embed(texts))
        elif hasattr(embedding_provider, 'passage_embed'):
            # Fallback for older versions (shouldn't exist but kept for safety)
            embeddings = list(embedding_provider.passage_embed(texts))
        else:
            raise AttributeError("FastEmbed provider has neither 'embed' nor 'passage_embed' method")
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
        if variance < 1e-4:  # Less strict threshold for valid embeddings
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
            "message_index": message_indices[0] if message_indices else None,
            "message_indices": message_indices  # Store all indices in this chunk
        }
        
        # Add metadata
        if metadata:
            payload.update(metadata)
        
        # Create point
        point = PointStruct(
            id=int(point_id, 16) % (2**63),
            vector=embedding,  # Use validated embedding variable
            payload=payload
        )
        
        # Upload with wait to ensure persistence (with retries)
        result = _with_retries(lambda: client.upsert(
            collection_name=collection_name,
            points=[point],
            wait=True  # Ensure operation completed before continuing
        ))
        
        # Verify the operation completed successfully (handle enum or string representations)
        status = getattr(result, 'status', None)
        if status and 'completed' not in str(status).lower():
            logger.error(f"Upsert not completed for {conversation_id}:{chunk_index}, status={status}")
            return 0
        
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
        "total_messages": 0,
        "project_path": None  # Add project path from cwd
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

                    # Extract cwd (current working directory) as project path
                    if metadata["project_path"] is None and 'cwd' in data:
                        metadata["project_path"] = data.get('cwd')

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
                                                if len(metadata['ast_elements']) < MAX_AST_ELEMENTS:
                                                    # Fix: More permissive regex to handle various fence formats
                                                    # Handles both ```\n and ```python\n cases, with optional newline
                                                    code_blocks = re.findall(r'```[^`\n]*\n?(.*?)```', item.get('text', ''), re.DOTALL)
                                                    for code_block in code_blocks[:MAX_CODE_BLOCKS]:  # Use defined constant
                                                        if len(metadata['ast_elements']) >= MAX_AST_ELEMENTS:
                                                            break
                                                        ast_elems = extract_ast_elements(code_block)
                                                        for elem in list(ast_elems)[:MAX_ELEMENTS_PER_BLOCK]:  # Use defined constant
                                                            if elem not in metadata['ast_elements'] and len(metadata['ast_elements']) < MAX_AST_ELEMENTS:
                                                                metadata['ast_elements'].append(elem)

                                        elif item.get('type') == 'thinking':
                                            # Also include thinking content in metadata extraction
                                            text_content += item.get('thinking', '')

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
        combined_text = ' '.join(all_text[:MAX_CONCEPT_MESSAGES])  # Limit messages for concept extraction
        metadata['concepts'] = extract_concepts(combined_text)

    # MANDATORY: AST-GREP Pattern Analysis
    # Analyze code quality for files mentioned in conversation
    pattern_quality = {}
    avg_quality_score = 0.0

    try:
        # Update patterns first (uses 24h cache, <100ms)
        from update_patterns import check_and_update_patterns
        check_and_update_patterns()

        # Import analyzer
        from ast_grep_final_analyzer import FinalASTGrepAnalyzer
        analyzer = FinalASTGrepAnalyzer()

        # Analyze edited and analyzed files
        files_to_analyze = list(set(metadata['files_edited'] + metadata['files_analyzed'][:10]))
        quality_scores = []

        for file_path in files_to_analyze:
            # Only analyze code files
            if file_path and any(file_path.endswith(ext) for ext in ['.py', '.ts', '.js', '.tsx', '.jsx']):
                try:
                    # Check if file exists and is accessible
                    if os.path.exists(file_path):
                        result = analyzer.analyze_file(file_path)
                        metrics = result['quality_metrics']
                        pattern_quality[file_path] = {
                            'score': metrics['quality_score'],
                            'good_patterns': metrics['good_patterns_found'],
                            'bad_patterns': metrics['bad_patterns_found'],
                            'issues': metrics['total_issues']
                        }
                        quality_scores.append(metrics['quality_score'])
                except Exception as e:
                    logger.debug(f"Could not analyze {file_path}: {e}")

        # Calculate average quality
        if quality_scores:
            avg_quality_score = sum(quality_scores) / len(quality_scores)

    except Exception as e:
        logger.debug(f"AST analysis not available: {e}")

    # Add pattern analysis to metadata
    metadata['pattern_analysis'] = pattern_quality
    metadata['avg_quality_score'] = round(avg_quality_score, 3)

    # Set total messages
    metadata['total_messages'] = message_count

    # Limit arrays
    metadata['files_analyzed'] = metadata['files_analyzed'][:MAX_FILES_ANALYZED]
    metadata['files_edited'] = metadata['files_edited'][:MAX_FILES_EDITED]
    metadata['tools_used'] = metadata['tools_used'][:MAX_TOOLS_USED]
    metadata['ast_elements'] = metadata['ast_elements'][:MAX_AST_ELEMENTS]

    return metadata, first_timestamp or datetime.now().isoformat(), message_count

def stream_import_file(jsonl_file: Path, collection_name: str, project_path: Path) -> int:
    """Stream import a single JSONL file without loading it into memory."""
    logger.info(f"Streaming import of {jsonl_file.name}")

    # Extract conversation ID
    conversation_id = jsonl_file.stem

    # Extract metadata in first pass (lightweight)
    metadata, created_at, total_messages = extract_metadata_single_pass(str(jsonl_file))

    # Track whether we should delete old points (only after successful import)
    should_delete_old = False
    
    # Reset counters for each conversation (critical for correct indexing)
    current_message_index = 0  # Must be reset before processing each conversation
    
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
                            # Extract content from various message types
                            content = msg['content']
                            if isinstance(content, list):
                                text_parts = []
                                for item in content:
                                    if isinstance(item, dict):
                                        item_type = item.get('type', '')
                                        if item_type == 'text':
                                            text_parts.append(item.get('text', ''))
                                        elif item_type == 'thinking':
                                            # Include thinking content (from Claude's thinking blocks)
                                            thinking_content = item.get('thinking', '')
                                            if thinking_content:
                                                text_parts.append(f"[Thinking] {thinking_content[:1000]}")  # Limit size
                                        elif item_type == 'tool_use':
                                            # Include tool use information
                                            tool_name = item.get('name', 'unknown')
                                            tool_input = str(item.get('input', ''))[:500]  # Limit size
                                            text_parts.append(f"[Tool: {tool_name}] {tool_input}")
                                        elif item_type == 'tool_result':
                                            # Include tool results
                                            result_content = str(item.get('content', ''))[:1000]  # Limit size
                                            text_parts.append(f"[Result] {result_content}")
                                    elif isinstance(item, str):
                                        text_parts.append(item)
                                content = '\n'.join(text_parts)
                            
                            if content:
                                # Track message index for user/assistant messages
                                if msg['role'] in ['user', 'assistant']:
                                    message_idx = current_message_index
                                    current_message_index += 1
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
                
                    # Handle top-level tool_result/tool_use events (no message wrapper)
                    entry_type = data.get('type')
                    if entry_type in ('tool_result', 'tool_use'):
                        text_parts = []
                        if entry_type == 'tool_use':
                            tool_name = data.get('name', 'unknown')
                            tool_input = str(data.get('input', ''))[:500]
                            text_parts.append(f"[Tool: {tool_name}] {tool_input}")
                        elif entry_type == 'tool_result':
                            # Common structures: either 'content' (list/str) or 'result'
                            result_content = data.get('content')
                            if isinstance(result_content, list):
                                # flatten to text
                                flat = []
                                for itm in result_content:
                                    if isinstance(itm, dict) and itm.get('type') == 'text':
                                        flat.append(itm.get('text', ''))
                                    elif isinstance(itm, str):
                                        flat.append(itm)
                                result_content = "\n".join(flat)
                            if not result_content:
                                result_content = data.get('result', '')  # fallback key used by some tools
                            text_parts.append(f"[Result] {str(result_content)[:1000]}")
                        
                        content = "\n".join([p for p in text_parts if p])
                        if content:
                            # Track message index for summary format too
                            message_idx = current_message_index
                            current_message_index += 1
                            
                            chunk_buffer.append({
                                'role': entry_type,
                                'content': content,
                                'message_index': message_idx
                            })
                            if len(chunk_buffer) >= MAX_CHUNK_SIZE:
                                chunks = process_and_upload_chunk(
                                    chunk_buffer, chunk_index, conversation_id,
                                    created_at, metadata, collection_name, project_path, total_messages
                                )
                                total_chunks += chunks
                                chunk_buffer = []
                                chunk_index += 1
                                gc.collect()
                                    
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

        # Only delete old points after successful import verification
        if total_chunks > 0:
            try:
                from qdrant_client.models import Filter, FieldCondition, MatchValue
                # Count old points before deletion for verification
                old_count_filter = Filter(
                    must=[FieldCondition(key="conversation_id", match=MatchValue(value=conversation_id))]
                )
                old_points = client.scroll(
                    collection_name=collection_name,
                    scroll_filter=old_count_filter,
                    limit=1
                )[0]

                if len(old_points) > total_chunks + 5:  # Allow some tolerance
                    # Only delete if we have significantly more old points than new
                    client.delete(
                        collection_name=collection_name,
                        points_selector=old_count_filter,
                        wait=True
                    )
                    logger.info(f"Deleted old points for conversation {conversation_id} after verifying new import")
            except Exception as e:
                logger.warning(f"Could not clean up old points for {conversation_id}: {e}")

        logger.info(f"Imported {total_chunks} chunks from {jsonl_file.name}")
        return total_chunks

    except Exception as e:
        logger.error(f"Failed to import {jsonl_file}: {e}")
        # Mark file as failed in state manager
        try:
            state_manager.mark_file_failed(str(jsonl_file), str(e))
        except Exception as state_error:
            logger.warning(f"Could not mark file as failed in state: {state_error}")
        return 0

def _with_retries(fn, attempts=3, base_sleep=0.5):
    """Execute function with retries and exponential backoff."""
    for i in range(attempts):
        try:
            return fn()
        except Exception as e:
            if i == attempts - 1:
                raise
            time.sleep(base_sleep * (2 ** i))
            logger.debug(f"Retrying after error: {e}")

def should_import_file(file_path: Path) -> bool:
    """Check if file should be imported using UnifiedStateManager."""
    try:
        # Get imported files from state manager
        imported_files = state_manager.get_imported_files()

        # Normalize the file path for comparison
        normalized_path = state_manager.normalize_path(str(file_path))

        if normalized_path in imported_files:
            file_info = imported_files[normalized_path]

            # Skip if file failed and we haven't reached retry limit
            if file_info.get("status") == "failed" and file_info.get("retry_count", 0) >= 3:
                logger.info(f"Skipping failed file (max retries reached): {file_path.name}")
                return False

            # Get file modification time for comparison
            last_modified = file_path.stat().st_mtime
            stored_modified = file_info.get("last_modified")

            # Check if file has been modified (convert stored timestamp to float if needed)
            if stored_modified:
                try:
                    # Parse ISO timestamp to float for comparison
                    stored_time = datetime.fromisoformat(stored_modified.replace("Z", "+00:00")).timestamp()
                    if abs(last_modified - stored_time) > 1:  # Allow 1 second tolerance
                        logger.info(f"File modified, will re-import: {file_path.name}")
                        return True
                except (ValueError, TypeError):
                    # If we can't parse the stored time, re-import to be safe
                    logger.warning(f"Could not parse stored modification time, will re-import: {file_path.name}")
                    return True

            # Check for suspiciously low chunk counts (likely failed imports)
            chunks = file_info.get("chunks", 0)
            file_size_kb = file_path.stat().st_size / 1024

            # Heuristic: Files > 10KB should have more than 2 chunks
            if file_size_kb > 10 and chunks <= 2 and file_info.get("status") != "failed":
                logger.warning(f"File has suspiciously low chunks ({chunks}) for size {file_size_kb:.1f}KB, will re-import: {file_path.name}")
                return True

            # Skip if successfully imported
            if file_info.get("status") == "completed":
                logger.info(f"Skipping successfully imported file: {file_path.name}")
                return False

        return True

    except Exception as e:
        logger.warning(f"Error checking import status for {file_path}: {e}")
        return True  # Default to importing if we can't check status

def update_file_state(file_path: Path, chunks: int, collection_name: str):
    """Update state for imported file using UnifiedStateManager."""
    try:
        # Determine embedding mode from collection suffix
        embedding_mode = "local" if collection_suffix == "local" else "cloud"

        # Add file to state manager
        state_manager.add_imported_file(
            file_path=str(file_path),
            chunks=chunks,
            importer="streaming",
            collection=collection_name,
            embedding_mode=embedding_mode,
            status="completed"
        )
        logger.debug(f"Updated state for {file_path.name}: {chunks} chunks")
    except Exception as e:
        logger.error(f"Failed to update state for {file_path}: {e}")

def main():
    """Main import function."""
    # Parse command-line arguments
    parser = argparse.ArgumentParser(description='Import conversations with unified embeddings support')
    parser.add_argument('--prefer-voyage', action='store_true', 
                       help='Use Voyage AI embeddings instead of local FastEmbed')
    parser.add_argument('--limit', type=int, 
                       help='Limit number of files to import')
    parser.add_argument('--max-files-per-cycle', type=int, 
                       help='Maximum files to process per cycle')
    args = parser.parse_args()
    
    # Override environment variable if --prefer-voyage is specified
    global PREFER_LOCAL_EMBEDDINGS, embedding_provider, embedding_dimension, collection_suffix
    if args.prefer_voyage:
        if not VOYAGE_API_KEY:
            logger.error("--prefer-voyage specified but VOYAGE_KEY environment variable not set")
            sys.exit(1)
        logger.info("Command-line flag --prefer-voyage detected, switching to Voyage AI embeddings")
        PREFER_LOCAL_EMBEDDINGS = False
        
        # Re-initialize embedding provider with Voyage
        import voyageai
        embedding_provider = voyageai.Client(api_key=VOYAGE_API_KEY)
        embedding_dimension = 1024
        collection_suffix = "voyage"
        logger.info("Switched to Voyage AI embeddings (dimension: 1024)")
    
    # Get status from state manager
    status = state_manager.get_status()
    logger.info(f"Loaded state with {status['indexed_files']} previously imported files")
    
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
    files_processed = 0
    
    for project_dir in project_dirs:
        # Get collection name
        collection_name = get_collection_name(project_dir)
        logger.info(f"Importing project: {project_dir.name} -> {collection_name}")
        
        # Ensure collection exists
        ensure_collection(collection_name)
        
        # Find JSONL files
        jsonl_files = sorted(project_dir.glob("*.jsonl"))
        
        # Apply limit from command line if specified
        if args.limit and files_processed >= args.limit:
            logger.info(f"Reached limit of {args.limit} files, stopping import")
            break
            
        # Limit files per cycle if specified
        max_files = args.max_files_per_cycle or int(os.getenv("MAX_FILES_PER_CYCLE", "1000"))
        jsonl_files = jsonl_files[:max_files]
        
        for jsonl_file in jsonl_files:
            # Check limit again per file
            if args.limit and files_processed >= args.limit:
                logger.info(f"Reached limit of {args.limit} files, stopping import")
                break
                
            if should_import_file(jsonl_file):
                chunks = stream_import_file(jsonl_file, collection_name, project_dir)
                files_processed += 1
                if chunks > 0:
                    # Verify data is actually in Qdrant before marking as imported
                    from qdrant_client.models import Filter, FieldCondition, MatchValue
                    try:
                        conversation_id = jsonl_file.stem
                        count_result = _with_retries(lambda: client.count(
                            collection_name=collection_name,
                            count_filter=Filter(
                                must=[FieldCondition(key="conversation_id", 
                                                   match=MatchValue(value=conversation_id))]
                            ),
                            exact=True  # Ensure exact count, not approximation
                        ))
                        actual_count = count_result.count if hasattr(count_result, 'count') else 0
                        
                        if actual_count > 0:
                            logger.info(f"Verified {actual_count} points in Qdrant for {conversation_id}")
                            update_file_state(jsonl_file, chunks, collection_name)
                            total_imported += 1
                        else:
                            logger.error(f"No points found in Qdrant for {conversation_id} despite {chunks} chunks processed - not marking as imported")
                    except Exception as e:
                        logger.error(f"Failed to verify Qdrant points for {jsonl_file.name}: {e}")
                        # Don't mark as imported if we can't verify
                    
                    # Force GC after each file
                    gc.collect()
                else:
                    # Critical fix: Don't mark files with 0 chunks as imported
                    # This allows retry on next run
                    logger.warning(f"File produced 0 chunks, not marking as imported: {jsonl_file.name}")
                    # Mark as failed so we don't keep retrying indefinitely
                    try:
                        state_manager.mark_file_failed(str(jsonl_file), "File produced 0 chunks during import")
                    except Exception as state_error:
                        logger.warning(f"Could not mark file as failed in state: {state_error}")
    
    logger.info(f"Import complete: processed {total_imported} files")

if __name__ == "__main__":
    main()