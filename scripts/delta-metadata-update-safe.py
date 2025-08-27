#!/usr/bin/env python3
"""
Safe delta metadata update script for Claude Self-Reflect.
Updates existing Qdrant points with tool usage metadata without overwhelming the system.
Includes rate limiting, batch processing, and proper error recovery.
"""

import os
import sys
import json
import hashlib
import re
import time
import asyncio
from datetime import datetime, timedelta
from typing import List, Dict, Any, Set, Tuple, Optional
import logging
from pathlib import Path
from collections import defaultdict

from qdrant_client import QdrantClient
from qdrant_client.models import Filter, FieldCondition, MatchValue

# Configuration
QDRANT_URL = os.getenv("QDRANT_URL", "http://localhost:6333")
LOGS_DIR = os.getenv("LOGS_DIR", os.path.expanduser("~/.claude/projects"))
# Use /config path if running in Docker, otherwise use ./config
STATE_FILE = os.getenv("STATE_FILE", "/config/delta-update-state.json" if os.path.exists("/config") else "./config/delta-update-state.json")
PREFER_LOCAL_EMBEDDINGS = os.getenv("PREFER_LOCAL_EMBEDDINGS", "true").lower() == "true"
DRY_RUN = os.getenv("DRY_RUN", "false").lower() == "true"
DAYS_TO_UPDATE = int(os.getenv("DAYS_TO_UPDATE", "7"))
BATCH_SIZE = int(os.getenv("BATCH_SIZE", "10"))  # Process N conversations at a time
RATE_LIMIT_DELAY = float(os.getenv("RATE_LIMIT_DELAY", "0.1"))  # Delay between updates
MAX_CONCURRENT_UPDATES = int(os.getenv("MAX_CONCURRENT_UPDATES", "5"))  # Max parallel updates

# Set up logging
logging.basicConfig(level=logging.INFO, format='%(asctime)s - %(levelname)s - %(message)s')
logger = logging.getLogger(__name__)

# Initialize Qdrant client
client = QdrantClient(url=QDRANT_URL, timeout=30)  # Increased timeout

def get_collection_suffix():
    """Get the collection suffix based on embedding type (for new collections only)."""
    return "_local" if PREFER_LOCAL_EMBEDDINGS else "_voyage"

def get_existing_collection_suffix(project_hash: str, max_retries: int = 3) -> str:
    """Detect which collection type actually exists for this project.
    
    This function checks for existing collections and returns the actual suffix found.
    Only falls back to preference when creating new collections.
    Includes retry logic for resilience against temporary Qdrant unavailability.
    
    Args:
        project_hash: The MD5 hash of the normalized project name
        max_retries: Maximum number of retry attempts for collection detection
        
    Returns:
        "_voyage" if voyage collection exists, "_local" if local exists,
        or preference-based suffix if neither exists yet
    """
    for attempt in range(max_retries):
        try:
            collections = client.get_collections().collections
            collection_names = [c.name for c in collections]
            
            # Check for both possible collection names
            voyage_name = f"conv_{project_hash}_voyage"
            local_name = f"conv_{project_hash}_local"
            
            # Return the actual collection type that exists
            if voyage_name in collection_names:
                logger.debug(f"Found existing Voyage collection: {voyage_name}")
                return "_voyage"
            elif local_name in collection_names:
                logger.debug(f"Found existing Local collection: {local_name}")
                return "_local"
            else:
                # No existing collection - use preference for new ones
                suffix = get_collection_suffix()
                logger.debug(f"No existing collection for hash {project_hash}, using preference: {suffix}")
                return suffix
        except Exception as e:
            if attempt < max_retries - 1:
                wait_time = 0.5 * (attempt + 1)  # Exponential backoff
                logger.debug(f"Error checking collections (attempt {attempt + 1}/{max_retries}): {e}, retrying in {wait_time}s")
                time.sleep(wait_time)
                continue
            logger.warning(f"Error checking collections after {max_retries} attempts: {e}, falling back to preference")
            return get_collection_suffix()

def normalize_project_name(project_name: str) -> str:
    """Normalize project name by removing path-like prefixes."""
    if project_name.startswith("-"):
        parts = project_name.split("-")
        for i, part in enumerate(parts):
            if part == "projects" and i < len(parts) - 1:
                return "-".join(parts[i+1:])
    return project_name

def normalize_path(path: str) -> str:
    """Normalize file paths for consistency across platforms."""
    if not path:
        return ""
    path = path.replace("/Users/", "~/").replace("\\Users\\", "~\\")
    path = path.replace("\\", "/")
    path = re.sub(r'/+', '/', path)
    return path

def extract_concepts(text: str, tool_usage: Dict[str, Any]) -> Set[str]:
    """Extract high-level concepts from conversation and tool usage."""
    concepts = set()
    
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
    
    combined_text = text.lower()
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

def extract_tool_usage_from_jsonl(jsonl_path: str) -> Dict[str, Any]:
    """Extract all tool usage from a conversation."""
    tool_usage = {
        "files_read": [],
        "files_edited": [],
        "files_created": [],
        "grep_searches": [],
        "bash_commands": [],
        "tools_summary": {}
    }
    
    try:
        with open(jsonl_path, 'r', encoding='utf-8') as f:
            for line in f:
                if not line.strip():
                    continue
                try:
                    data = json.loads(line)
                    if 'message' in data and data['message']:
                        msg = data['message']
                        if msg.get('role') == 'assistant' and msg.get('content'):
                            content = msg['content']
                            if isinstance(content, list):
                                for item in content:
                                    if isinstance(item, dict) and item.get('type') == 'tool_use':
                                        tool_name = item.get('name', '')
                                        inputs = item.get('input', {})
                                        
                                        # Track tool usage
                                        tool_usage['tools_summary'][tool_name] = tool_usage['tools_summary'].get(tool_name, 0) + 1
                                        
                                        # Extract file paths
                                        if tool_name == 'Read':
                                            file_path = inputs.get('file_path')
                                            if file_path:
                                                tool_usage['files_read'].append(normalize_path(file_path))
                                        elif tool_name in ['Edit', 'Write', 'MultiEdit']:
                                            file_path = inputs.get('file_path')
                                            if file_path:
                                                tool_usage['files_edited'].append(normalize_path(file_path))
                                        elif tool_name == 'Grep':
                                            pattern = inputs.get('pattern')
                                            if pattern:
                                                tool_usage['grep_searches'].append({'pattern': pattern[:100]})
                                        elif tool_name == 'Bash':
                                            command = inputs.get('command', '')[:200]
                                            if command:
                                                tool_usage['bash_commands'].append(command)
                except Exception as e:
                    continue
    except Exception as e:
        logger.error(f"Error reading JSONL file {jsonl_path}: {e}")
    
    # Deduplicate
    tool_usage['files_read'] = list(set(tool_usage['files_read']))[:20]
    tool_usage['files_edited'] = list(set(tool_usage['files_edited']))[:10]
    
    return tool_usage

def load_state() -> Dict[str, Any]:
    """Load the current state from file."""
    state_path = Path(STATE_FILE)
    if state_path.exists():
        try:
            with open(state_path, 'r') as f:
                return json.load(f)
        except Exception as e:
            logger.warning(f"Could not load state: {e}")
    return {
        "last_update": None,
        "updated_conversations": {},
        "failed_conversations": {}
    }

def save_state(state: Dict[str, Any]):
    """Save the current state to file."""
    state_path = Path(STATE_FILE)
    state_path.parent.mkdir(parents=True, exist_ok=True)
    
    state["last_update"] = datetime.now().isoformat()
    
    try:
        with open(state_path, 'w') as f:
            json.dump(state, f, indent=2)
    except Exception as e:
        logger.error(f"Could not save state: {e}")

async def update_point_metadata_batch(updates: List[Tuple[str, int, Dict, str]]) -> int:
    """Update multiple points in a batch with rate limiting."""
    success_count = 0
    
    for conversation_id, chunk_index, metadata, collection_name in updates:
        try:
            # Calculate point ID
            point_id_str = hashlib.md5(
                f"{conversation_id}_{chunk_index}".encode()
            ).hexdigest()[:16]
            point_id = int(point_id_str, 16) % (2**63)
            
            if not DRY_RUN:
                # Update with rate limiting
                client.set_payload(
                    collection_name=collection_name,
                    payload=metadata,
                    points=[point_id],
                    wait=False
                )
                success_count += 1
                
                # Rate limit to avoid overwhelming Qdrant
                await asyncio.sleep(RATE_LIMIT_DELAY)
            else:
                logger.info(f"[DRY RUN] Would update point {point_id}")
                success_count += 1
                
        except Exception as e:
            logger.debug(f"Failed to update point {conversation_id}_{chunk_index}: {e}")
    
    return success_count

async def process_conversation_async(jsonl_file: Path, state: Dict[str, Any]) -> bool:
    """Process a single conversation file asynchronously."""
    try:
        conversation_id = jsonl_file.stem
        project_name = jsonl_file.parent.name
        
        # Check if already updated
        if conversation_id in state.get("updated_conversations", {}):
            last_updated = state["updated_conversations"][conversation_id].get("updated_at")
            file_mtime = jsonl_file.stat().st_mtime
            if last_updated and last_updated >= file_mtime:
                logger.debug(f"Skipping {conversation_id} - already updated")
                return True
        
        # Check if previously failed too many times
        failed_info = state.get("failed_conversations", {}).get(conversation_id, {})
        if failed_info.get("retry_count", 0) > 3:
            logger.debug(f"Skipping {conversation_id} - too many failures")
            return False
        
        logger.info(f"Processing: {conversation_id}")
        
        # Extract metadata
        tool_usage = extract_tool_usage_from_jsonl(str(jsonl_file))
        
        # Read conversation text (limited)
        conversation_text = ""
        with open(jsonl_file, 'r', encoding='utf-8') as f:
            for i, line in enumerate(f):
                if i > 100:  # Limit lines to avoid memory issues
                    break
                if line.strip():
                    try:
                        data = json.loads(line)
                        if 'message' in data and data['message']:
                            msg = data['message']
                            if msg.get('content'):
                                if isinstance(msg['content'], str):
                                    conversation_text += msg['content'][:500] + "\n"
                    except Exception as e:
                        logger.debug(f"Parse error in {jsonl_file}: {e}")
                        continue
        
        # Extract concepts
        concepts = extract_concepts(conversation_text[:10000], tool_usage)
        
        # Prepare metadata
        metadata_update = {
            "files_analyzed": tool_usage.get('files_read', [])[:20],
            "files_edited": tool_usage.get('files_edited', [])[:10],
            "tools_used": list(tool_usage.get('tools_summary', {}).keys())[:20],
            "concepts": list(concepts)[:15],
            "has_file_metadata": True,
            "metadata_updated_at": datetime.now().isoformat()
        }
        
        # Determine collection
        project_hash = hashlib.md5(normalize_project_name(project_name).encode()).hexdigest()[:8]
        # Use smart detection to find the actual collection type
        collection_suffix = get_existing_collection_suffix(project_hash)
        collection_name = f"conv_{project_hash}{collection_suffix}"
        
        # Check if collection exists
        try:
            collections = client.get_collections().collections
            if collection_name not in [c.name for c in collections]:
                logger.warning(f"Collection {collection_name} not found")
                return False
        except Exception as e:
            logger.error(f"Error checking collection: {e}")
            # Record failure
            state.setdefault("failed_conversations", {})[conversation_id] = {
                "error": str(e),
                "retry_count": failed_info.get("retry_count", 0) + 1,
                "last_attempt": time.time()
            }
            return False
        
        # Prepare batch updates
        updates = []
        for chunk_index in range(20):  # Most conversations have < 20 chunks
            updates.append((conversation_id, chunk_index, metadata_update, collection_name))
        
        # Process in batch with rate limiting
        success_count = await update_point_metadata_batch(updates)
        
        if success_count > 0:
            logger.info(f"Updated {success_count} chunks for {conversation_id}")
            state["updated_conversations"][conversation_id] = {
                "updated_at": time.time(),
                "chunks_updated": success_count,
                "project": project_name
            }
            return True
        else:
            logger.warning(f"No chunks updated for {conversation_id}")
            return False
            
    except Exception as e:
        logger.error(f"Failed to process {jsonl_file}: {e}")
        return False

async def main_async():
    """Main async function with proper batching and rate limiting."""
    logger.info("=== Starting Safe Delta Metadata Update ===")
    logger.info(f"Configuration:")
    logger.info(f"  Qdrant URL: {QDRANT_URL}")
    logger.info(f"  Days to update: {DAYS_TO_UPDATE}")
    logger.info(f"  Batch size: {BATCH_SIZE}")
    logger.info(f"  Rate limit delay: {RATE_LIMIT_DELAY}s")
    logger.info(f"  Max concurrent: {MAX_CONCURRENT_UPDATES}")
    
    # Load state
    state = load_state()
    
    # Get recent files
    recent_files = []
    cutoff_time = datetime.now() - timedelta(days=DAYS_TO_UPDATE)
    logs_path = Path(LOGS_DIR)
    
    if logs_path.exists():
        for jsonl_file in logs_path.glob("**/*.jsonl"):
            try:
                mtime = datetime.fromtimestamp(jsonl_file.stat().st_mtime)
                if mtime >= cutoff_time:
                    recent_files.append(jsonl_file)
            except:
                continue
    
    logger.info(f"Found {len(recent_files)} conversations from the past {DAYS_TO_UPDATE} days")
    
    # Process in batches
    success_count = 0
    failed_count = 0
    
    for i in range(0, len(recent_files), BATCH_SIZE):
        batch = recent_files[i:i + BATCH_SIZE]
        logger.info(f"Processing batch {i//BATCH_SIZE + 1}/{(len(recent_files) + BATCH_SIZE - 1)//BATCH_SIZE}")
        
        # Create tasks for concurrent processing
        tasks = []
        for jsonl_file in batch:
            task = asyncio.create_task(process_conversation_async(jsonl_file, state))
            tasks.append(task)
        
        # Wait for batch to complete
        results = await asyncio.gather(*tasks)
        
        # Count results
        batch_success = sum(1 for r in results if r)
        batch_failed = len(results) - batch_success
        success_count += batch_success
        failed_count += batch_failed
        
        # Save state after each batch
        save_state(state)
        
        # Add delay between batches to avoid overwhelming the system
        if i + BATCH_SIZE < len(recent_files):
            await asyncio.sleep(1.0)
    
    # Final save
    save_state(state)
    
    logger.info("=== Delta Update Complete ===")
    logger.info(f"Successfully updated: {success_count} conversations")
    logger.info(f"Failed: {failed_count} conversations")
    logger.info(f"Total conversations in state: {len(state.get('updated_conversations', {}))}")

def main():
    """Entry point."""
    asyncio.run(main_async())

if __name__ == "__main__":
    main()