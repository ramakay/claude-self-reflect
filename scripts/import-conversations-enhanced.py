#!/usr/bin/env python3
"""
Enhanced import script that extracts tool usage metadata from conversations.
Supports both local and Voyage AI embeddings with tool tracking.
"""

import os
import sys
import json
import glob
import hashlib
import gc
import re
import time
from datetime import datetime, timedelta
from typing import List, Dict, Any, Set, Tuple
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
STATE_FILE = os.getenv("STATE_FILE", "./config/imported-files-enhanced.json")
BATCH_SIZE = int(os.getenv("BATCH_SIZE", "10"))
PREFER_LOCAL_EMBEDDINGS = os.getenv("PREFER_LOCAL_EMBEDDINGS", "false").lower() == "true"
VOYAGE_API_KEY = os.getenv("VOYAGE_KEY")
DRY_RUN = os.getenv("DRY_RUN", "false").lower() == "true"

# Set up logging
logging.basicConfig(level=logging.INFO, format='%(asctime)s - %(levelname)s - %(message)s')
logger = logging.getLogger(__name__)

# Import timing stats
timing_stats = {
    "extract": [],
    "chunk": [],
    "embed": [],
    "store": [],
    "total": []
}

def normalize_path(path: str) -> str:
    """Normalize file paths for consistency across platforms."""
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
        'database': r'(database|SQL|query|migration|schema|postgres|mysql|mongodb)',
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
    
    # Check text content
    combined_text = text.lower()
    for concept, pattern in concept_patterns.items():
        if re.search(pattern, combined_text, re.IGNORECASE):
            concepts.add(concept)
    
    # Check tool usage patterns
    tool_text = json.dumps(tool_usage).lower()
    for concept, pattern in concept_patterns.items():
        if re.search(pattern, tool_text, re.IGNORECASE):
            concepts.add(concept)
    
    # Add concepts based on specific tool usage
    if tool_usage.get('grep_searches'):
        concepts.add('search')
    if tool_usage.get('files_edited') or tool_usage.get('files_created'):
        concepts.add('development')
    if any('test' in str(f).lower() for f in tool_usage.get('files_read', [])):
        concepts.add('testing')
    if any('docker' in str(cmd).lower() for cmd in tool_usage.get('bash_commands', [])):
        concepts.add('docker')
    
    return concepts

def extract_tool_usage_from_jsonl(jsonl_path: str) -> Dict[str, Any]:
    """Extract all tool usage from a conversation."""
    tool_usage = {
        "files_read": [],
        "files_edited": [],
        "files_created": [],
        "grep_searches": [],
        "bash_commands": [],
        "glob_patterns": [],
        "task_calls": [],
        "mcp_calls": [],
        "tools_summary": {},
        "concepts": set(),
        "timing": {},
        "errors": [],
        "tool_results": {}
    }
    
    start_time = time.time()
    
    with open(jsonl_path, 'r', encoding='utf-8') as f:
        for line_num, line in enumerate(f, 1):
            line = line.strip()
            if not line:
                continue
            
            try:
                data = json.loads(line)
                
                # Skip API error messages
                if data.get('isApiErrorMessage'):
                    continue
                
                # Process message content
                if 'message' in data and 'content' in data['message']:
                    content = data['message']['content']
                    
                    # Handle content array (where tool_use lives)
                    if isinstance(content, list):
                        for item in content:
                            if isinstance(item, dict) and item.get('type') == 'tool_use':
                                extract_single_tool_use(item, tool_usage)
                                
            except json.JSONDecodeError as e:
                logger.debug(f"Skipping invalid JSON at line {line_num}: {e}")
            except Exception as e:
                logger.error(f"Error processing line {line_num}: {e}")
                tool_usage["errors"].append({"line": line_num, "error": str(e)})
    
    # Calculate timing
    tool_usage["timing"]["extract_ms"] = int((time.time() - start_time) * 1000)
    
    # Convert sets to lists for JSON serialization
    tool_usage["concepts"] = list(tool_usage["concepts"])
    
    return tool_usage

def extract_single_tool_use(tool_data: Dict[str, Any], usage_dict: Dict[str, Any]) -> None:
    """Parse individual tool usage with enhanced metadata extraction."""
    tool_name = tool_data.get('name')
    inputs = tool_data.get('input', {})
    tool_id = tool_data.get('id')
    
    # Track tool frequency
    usage_dict['tools_summary'][tool_name] = usage_dict['tools_summary'].get(tool_name, 0) + 1
    
    # Extract based on tool type
    if tool_name == 'Read':
        path = inputs.get('file_path')
        if path:
            usage_dict['files_read'].append({
                'path': normalize_path(path),
                'offset': inputs.get('offset', 0),
                'limit': inputs.get('limit', -1),
                'tool_id': tool_id
            })
    
    elif tool_name == 'Grep':
        pattern = inputs.get('pattern')
        if pattern:
            usage_dict['grep_searches'].append({
                'pattern': pattern[:100],  # Limit pattern length
                'path': normalize_path(inputs.get('path', '.')),
                'glob': inputs.get('glob'),
                'output_mode': inputs.get('output_mode', 'files_with_matches'),
                'case_insensitive': inputs.get('-i', False)
            })
            # Add search concept
            usage_dict['concepts'].add('search')
    
    elif tool_name == 'Edit' or tool_name == 'MultiEdit':
        path = inputs.get('file_path')
        if path:
            usage_dict['files_edited'].append({
                'path': normalize_path(path),
                'operation': tool_name.lower()
            })
    
    elif tool_name == 'Write':
        path = inputs.get('file_path')
        if path:
            usage_dict['files_created'].append(normalize_path(path))
    
    elif tool_name == 'Bash':
        cmd = inputs.get('command', '')
        if cmd:
            # Extract command name
            cmd_parts = cmd.split()
            cmd_name = cmd_parts[0] if cmd_parts else 'unknown'
            
            usage_dict['bash_commands'].append({
                'command': cmd_name,
                'description': inputs.get('description', '')[:100]
            })
            
            # Add concepts based on commands
            if 'docker' in cmd.lower():
                usage_dict['concepts'].add('docker')
            if 'git' in cmd.lower():
                usage_dict['concepts'].add('git')
            if 'test' in cmd.lower() or 'pytest' in cmd.lower():
                usage_dict['concepts'].add('testing')
    
    elif tool_name == 'Glob':
        pattern = inputs.get('pattern')
        if pattern:
            usage_dict['glob_patterns'].append({
                'pattern': pattern,
                'path': normalize_path(inputs.get('path', '.'))
            })
    
    elif tool_name == 'Task':
        usage_dict['task_calls'].append({
            'description': inputs.get('description', '')[:100],
            'subagent_type': inputs.get('subagent_type')
        })
    
    # Handle MCP tools
    elif tool_name and tool_name.startswith('mcp__'):
        usage_dict['mcp_calls'].append({
            'tool': tool_name,
            'params': list(inputs.keys()) if inputs else []
        })
        usage_dict['concepts'].add('mcp')

def create_enhanced_chunk(messages: List[Dict], chunk_index: int, tool_usage: Dict[str, Any], 
                         conversation_metadata: Dict[str, Any]) -> Dict[str, Any]:
    """Create chunk with tool usage metadata."""
    # Extract text from messages
    chunk_text = "\n\n".join([
        f"{msg['role'].upper()}: {msg['content']}"
        for msg in messages
    ])
    
    # Extract concepts from chunk text and tool usage
    concepts = extract_concepts(chunk_text, tool_usage)
    
    # Deduplicate and clean file paths
    all_file_items = tool_usage.get('files_read', []) + tool_usage.get('files_edited', [])
    files_analyzed = list(set([
        item['path'] if isinstance(item, dict) else item
        for item in all_file_items
        if (isinstance(item, dict) and item.get('path')) or isinstance(item, str)
    ]))[:20]  # Limit to 20 files
    
    files_edited = list(set([
        item['path'] if isinstance(item, dict) else item
        for item in tool_usage.get('files_edited', [])
        if (isinstance(item, dict) and item.get('path')) or isinstance(item, str)
    ]))[:10]  # Limit to 10 files
    
    # Build enhanced chunk
    chunk = {
        "text": chunk_text,
        "conversation_id": conversation_metadata['id'],
        "chunk_index": chunk_index,
        "timestamp": conversation_metadata['timestamp'],
        "project": conversation_metadata['project'],
        "start_role": messages[0]['role'] if messages else 'unknown',
        
        # Tool usage metadata
        "files_analyzed": files_analyzed,
        "files_edited": files_edited,
        "search_patterns": [s['pattern'] for s in tool_usage.get('grep_searches', [])][:10],
        "concepts": list(concepts)[:15],
        "tool_summary": dict(list(tool_usage.get('tools_summary', {}).items())[:10]),
        "analysis_only": len(tool_usage.get('files_edited', [])) == 0 and len(tool_usage.get('files_created', [])) == 0,
        
        # Additional context
        "commands_used": list(set([c['command'] for c in tool_usage.get('bash_commands', [])]))[:10],
        "has_security_check": 'security' in concepts,
        "has_performance_check": 'performance' in concepts,
        "mcp_tools_used": list(set([m['tool'].split('__')[1] if '__' in m['tool'] else m['tool'] 
                                   for m in tool_usage.get('mcp_calls', [])]))[:5]
    }
    
    return chunk

# Import state management functions (same as original)
def load_state():
    """Load the import state from file."""
    if os.path.exists(STATE_FILE):
        try:
            with open(STATE_FILE, 'r') as f:
                state = json.load(f)
                if "imported_files" not in state:
                    state["imported_files"] = {}
                return state
        except Exception as e:
            logger.warning(f"Failed to load state file: {e}")
    return {"imported_files": {}}

def save_state(state):
    """Save the import state to file."""
    try:
        os.makedirs(os.path.dirname(STATE_FILE), exist_ok=True)
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
        last_imported = state["imported_files"][str_path].get("last_imported", 0)
        last_modified = state["imported_files"][str_path].get("last_modified", 0)
        
        if file_mtime <= last_modified and last_imported > 0:
            logger.info(f"Skipping unchanged file: {file_path.name}")
            return False
    
    return True

def update_file_state(file_path, state, chunks_imported, tool_stats=None):
    """Update the state for an imported file with tool usage stats."""
    str_path = str(file_path)
    state["imported_files"][str_path] = {
        "last_modified": os.path.getmtime(file_path),
        "last_imported": datetime.now().timestamp(),
        "chunks_imported": chunks_imported,
        "tool_stats": tool_stats or {}
    }

# Initialize embedding provider
embedding_provider = None
embedding_dimension = None
collection_suffix = None

if PREFER_LOCAL_EMBEDDINGS or not VOYAGE_API_KEY:
    logger.info("Using local FastEmbed embeddings")
    from fastembed import TextEmbedding
    embedding_provider = TextEmbedding(model_name="sentence-transformers/all-MiniLM-L6-v2")
    embedding_dimension = 384
    collection_suffix = "_local"
else:
    logger.info("Using Voyage AI embeddings")
    import voyageai
    vo = voyageai.Client(api_key=VOYAGE_API_KEY)
    embedding_provider = vo
    embedding_dimension = 1024
    collection_suffix = "_voyage"

# Initialize Qdrant client
client = QdrantClient(url=QDRANT_URL)

def chunk_conversation(messages: List[Dict], chunk_size: int = 10) -> List[Dict]:
    """Split conversation into chunks of messages."""
    chunks = []
    for i in range(0, len(messages), chunk_size):
        chunk_messages = messages[i:i + chunk_size]
        chunks.append({
            "messages": chunk_messages,
            "chunk_index": i // chunk_size
        })
    return chunks

@retry(stop=stop_after_attempt(3), wait=wait_random_exponential(min=1, max=20))
def generate_embeddings(texts: List[str]) -> List[List[float]]:
    """Generate embeddings for texts with retry logic."""
    if PREFER_LOCAL_EMBEDDINGS or not VOYAGE_API_KEY:
        embeddings = list(embedding_provider.embed(texts))
        return [emb.tolist() if hasattr(emb, 'tolist') else emb for emb in embeddings]
    else:
        result = embedding_provider.embed(texts, model="voyage-3", input_type="document")
        return result.embeddings

def import_project(project_path: Path, state: Dict) -> int:
    """Import conversations from a single project with tool usage extraction."""
    total_chunks = 0
    jsonl_files = list(project_path.glob("*.jsonl"))
    
    if not jsonl_files:
        return 0
    
    # Create or verify collection
    collection_name = f"conv_{hashlib.md5(project_path.name.encode()).hexdigest()[:8]}{collection_suffix}"
    
    try:
        collections = [c.name for c in client.get_collections().collections]
        if collection_name not in collections:
            client.create_collection(
                collection_name=collection_name,
                vectors_config=VectorParams(size=embedding_dimension, distance=Distance.COSINE)
            )
            logger.info(f"Created collection: {collection_name}")
    except Exception as e:
        logger.error(f"Failed to create/verify collection {collection_name}: {e}")
        return 0
    
    for jsonl_file in jsonl_files:
        if not should_import_file(jsonl_file, state):
            continue
            
        logger.info(f"Processing file: {jsonl_file.name}")
        
        try:
            file_start_time = time.time()
            
            # Extract tool usage
            extract_start = time.time()
            tool_usage = extract_tool_usage_from_jsonl(str(jsonl_file))
            extract_time = time.time() - extract_start
            timing_stats["extract"].append(extract_time)
            
            # Read and process messages (original logic)
            messages = []
            created_at = None
            
            with open(jsonl_file, 'r', encoding='utf-8') as f:
                for line_num, line in enumerate(f, 1):
                    line = line.strip()
                    if not line:
                        continue
                    
                    try:
                        data = json.loads(line)
                        
                        if created_at is None and 'timestamp' in data:
                            created_at = data.get('timestamp')
                        
                        if data.get('type') == 'summary':
                            continue
                            
                        if 'message' in data and data['message']:
                            msg = data['message']
                            if msg.get('role') and msg.get('content'):
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
                    except Exception as e:
                        logger.error(f"Error processing line {line_num}: {e}")
            
            if not messages:
                continue
            
            # Prepare metadata
            if created_at is None:
                created_at = datetime.now().isoformat()
            conversation_id = jsonl_file.stem
            
            conversation_metadata = {
                'id': conversation_id,
                'timestamp': created_at,
                'project': project_path.name
            }
            
            # Chunk the conversation
            chunk_start = time.time()
            chunks_data = chunk_conversation(messages)
            enhanced_chunks = []
            
            for chunk_data in chunks_data:
                enhanced_chunk = create_enhanced_chunk(
                    chunk_data["messages"],
                    chunk_data["chunk_index"],
                    tool_usage,
                    conversation_metadata
                )
                enhanced_chunks.append(enhanced_chunk)
            
            chunk_time = time.time() - chunk_start
            timing_stats["chunk"].append(chunk_time)
            
            if not enhanced_chunks:
                continue
            
            # Process in batches
            for batch_start in range(0, len(enhanced_chunks), BATCH_SIZE):
                batch = enhanced_chunks[batch_start:batch_start + BATCH_SIZE]
                texts = [chunk["text"] for chunk in batch]
                
                # Generate embeddings
                embed_start = time.time()
                embeddings = generate_embeddings(texts)
                embed_time = time.time() - embed_start
                timing_stats["embed"].append(embed_time)
                
                # Create points
                points = []
                for chunk, embedding in zip(batch, embeddings):
                    point_id = hashlib.md5(
                        f"{conversation_id}_{chunk['chunk_index']}".encode()
                    ).hexdigest()[:16]
                    
                    points.append(PointStruct(
                        id=int(point_id, 16) % (2**63),
                        vector=embedding,
                        payload=chunk
                    ))
                
                # Upload to Qdrant (unless dry run)
                if not DRY_RUN:
                    store_start = time.time()
                    client.upsert(
                        collection_name=collection_name,
                        points=points
                    )
                    store_time = time.time() - store_start
                    timing_stats["store"].append(store_time)
                else:
                    logger.info(f"[DRY RUN] Would upload {len(points)} points to {collection_name}")
                
                total_chunks += len(points)
            
            file_chunks = len(enhanced_chunks)
            total_time = time.time() - file_start_time
            timing_stats["total"].append(total_time)
            
            logger.info(f"Imported {file_chunks} chunks from {jsonl_file.name} "
                       f"(extract: {extract_time:.2f}s, chunk: {chunk_time:.2f}s, total: {total_time:.2f}s)")
            
            # Update state with tool stats
            tool_stats = {
                "tools_used": list(tool_usage['tools_summary'].keys()),
                "files_analyzed": len(enhanced_chunks[0].get('files_analyzed', [])) if enhanced_chunks else 0,
                "concepts": list(tool_usage.get('concepts', []))[:10]
            }
            update_file_state(jsonl_file, state, file_chunks, tool_stats)
            
            # Save state after each file
            if not DRY_RUN:
                save_state(state)
            
            gc.collect()
            
        except Exception as e:
            logger.error(f"Failed to import {jsonl_file}: {e}")
            import traceback
            logger.error(traceback.format_exc())
    
    return total_chunks

def main():
    """Main import function with enhanced features."""
    import argparse
    
    parser = argparse.ArgumentParser(description='Import conversations with tool usage extraction')
    parser.add_argument('--days', type=int, help='Import only files from last N days')
    parser.add_argument('--limit', type=int, help='Limit number of files to import')
    parser.add_argument('--dry-run', action='store_true', help='Run without actually importing')
    parser.add_argument('--project', type=str, help='Import only specific project')
    
    args = parser.parse_args()
    
    if args.dry_run:
        global DRY_RUN
        DRY_RUN = True
        logger.info("Running in DRY RUN mode - no data will be imported")
    
    logs_path = Path(LOGS_DIR)
    
    # Handle local development vs Docker paths
    if not logs_path.exists():
        # Try local development path
        home_logs = Path.home() / '.claude' / 'projects'
        if home_logs.exists():
            logs_path = home_logs
            logger.info(f"Using local logs directory: {logs_path}")
        else:
            logger.error(f"Logs directory not found: {LOGS_DIR}")
            return
    
    # Load existing state
    state = load_state()
    logger.info(f"Loaded state with {len(state['imported_files'])} previously imported files")
    
    # Find project directories
    if args.project:
        project_dirs = [d for d in logs_path.iterdir() if d.is_dir() and args.project in d.name]
    else:
        project_dirs = [d for d in logs_path.iterdir() if d.is_dir()]
    
    if not project_dirs:
        logger.warning("No project directories found")
        return
    
    # Filter by date if specified
    if args.days:
        cutoff_date = datetime.now() - timedelta(days=args.days)
        filtered_dirs = []
        for project_dir in project_dirs:
            jsonl_files = list(project_dir.glob("*.jsonl"))
            recent_files = [f for f in jsonl_files if datetime.fromtimestamp(f.stat().st_mtime) > cutoff_date]
            if recent_files:
                filtered_dirs.append(project_dir)
        project_dirs = filtered_dirs
        logger.info(f"Filtered to {len(project_dirs)} projects with files from last {args.days} days")
    
    # Apply limit if specified
    if args.limit:
        project_dirs = project_dirs[:args.limit]
    
    logger.info(f"Found {len(project_dirs)} projects to import")
    
    # Import each project
    total_imported = 0
    for project_dir in project_dirs:
        logger.info(f"Importing project: {project_dir.name}")
        chunks = import_project(project_dir, state)
        total_imported += chunks
    
    # Print timing statistics
    logger.info("\n=== Import Performance Summary ===")
    logger.info(f"Total chunks imported: {total_imported}")
    
    if timing_stats["total"]:
        logger.info(f"\nTiming averages:")
        logger.info(f"  Extract: {sum(timing_stats['extract'])/len(timing_stats['extract']):.2f}s")
        logger.info(f"  Chunk: {sum(timing_stats['chunk'])/len(timing_stats['chunk']):.2f}s")
        if timing_stats['embed']:
            logger.info(f"  Embed: {sum(timing_stats['embed'])/len(timing_stats['embed']):.2f}s")
        if timing_stats['store']:
            logger.info(f"  Store: {sum(timing_stats['store'])/len(timing_stats['store']):.2f}s")
        logger.info(f"  Total: {sum(timing_stats['total'])/len(timing_stats['total']):.2f}s per file")

if __name__ == "__main__":
    main()