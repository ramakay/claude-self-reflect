#!/usr/bin/env python3
"""
Delta metadata update script for Claude Self-Reflect.
Updates existing Qdrant points with tool usage metadata without re-importing vectors.
This allows us to enhance past conversations with file tracking and concept extraction.
"""

import os
import sys
import json
import hashlib
import re
import time
from datetime import datetime, timedelta
from typing import List, Dict, Any, Set, Tuple, Optional
import logging
from pathlib import Path

from qdrant_client import QdrantClient
from qdrant_client.models import Filter, FieldCondition, MatchValue

# Configuration
QDRANT_URL = os.getenv("QDRANT_URL", "http://localhost:6333")
LOGS_DIR = os.getenv("LOGS_DIR", "/Users/ramakrishnanannaswamy/.claude/projects")
STATE_FILE = os.getenv("STATE_FILE", "./config/delta-update-state.json")
PREFER_LOCAL_EMBEDDINGS = os.getenv("PREFER_LOCAL_EMBEDDINGS", "true").lower() == "true"
DRY_RUN = os.getenv("DRY_RUN", "false").lower() == "true"
DAYS_TO_UPDATE = int(os.getenv("DAYS_TO_UPDATE", "7"))

# Set up logging
logging.basicConfig(level=logging.INFO, format='%(asctime)s - %(levelname)s - %(message)s')
logger = logging.getLogger(__name__)

# Initialize Qdrant client
client = QdrantClient(url=QDRANT_URL)

def get_collection_suffix():
    """Get the collection suffix based on embedding type."""
    return "_local" if PREFER_LOCAL_EMBEDDINGS else "_voyage"

def normalize_project_name(project_name: str) -> str:
    """Normalize project name by removing path-like prefixes."""
    # Remove path-like prefixes (e.g., "-Users-username-projects-")
    if project_name.startswith("-"):
        # Split by '-' and reconstruct
        parts = project_name.split("-")
        # Find where the actual project name starts (usually after 'projects')
        for i, part in enumerate(parts):
            if part == "projects" and i < len(parts) - 1:
                return "-".join(parts[i+1:])
    return project_name

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
    
    # Check text content
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
        "glob_patterns": [],
        "task_calls": [],
        "web_searches": [],
        "tools_summary": {}
    }
    
    try:
        with open(jsonl_path, 'r', encoding='utf-8') as f:
            for line in f:
                if not line.strip():
                    continue
                
                try:
                    data = json.loads(line)
                    
                    # Look for tool usage in message content
                    if 'message' in data and data['message']:
                        msg = data['message']
                        if msg.get('role') == 'assistant' and msg.get('content'):
                            content = msg['content']
                            
                            # Handle content as list of objects
                            if isinstance(content, list):
                                for item in content:
                                    if isinstance(item, dict) and item.get('type') == 'tool_use':
                                        extract_tool_data(item, tool_usage)
                            # Handle content as string (legacy format)
                            elif isinstance(content, str):
                                # Try to extract tool usage from text patterns
                                extract_tools_from_text(content, tool_usage)
                                
                except json.JSONDecodeError:
                    continue
                except Exception as e:
                    logger.debug(f"Error processing line: {e}")
    
    except Exception as e:
        logger.error(f"Error reading JSONL file {jsonl_path}: {e}")
    
    # Calculate tools summary
    all_tools = []
    if tool_usage['files_read']:
        all_tools.extend(['Read'] * len(tool_usage['files_read']))
    if tool_usage['files_edited']:
        all_tools.extend(['Edit'] * len(tool_usage['files_edited']))
    if tool_usage['files_created']:
        all_tools.extend(['Write'] * len(tool_usage['files_created']))
    if tool_usage['grep_searches']:
        all_tools.extend(['Grep'] * len(tool_usage['grep_searches']))
    if tool_usage['bash_commands']:
        all_tools.extend(['Bash'] * len(tool_usage['bash_commands']))
    if tool_usage['glob_patterns']:
        all_tools.extend(['Glob'] * len(tool_usage['glob_patterns']))
    if tool_usage['task_calls']:
        all_tools.extend(['Task'] * len(tool_usage['task_calls']))
    if tool_usage['web_searches']:
        all_tools.extend(['WebSearch'] * len(tool_usage['web_searches']))
    
    # Count tool usage
    for tool in all_tools:
        tool_usage['tools_summary'][tool] = tool_usage['tools_summary'].get(tool, 0) + 1
    
    return tool_usage

def extract_tool_data(tool_use: Dict[str, Any], usage_dict: Dict[str, Any]):
    """Extract tool usage data from a tool_use object."""
    tool_name = tool_use.get('name', '')
    inputs = tool_use.get('input', {})
    
    # Handle Read tool
    if tool_name == 'Read':
        file_path = inputs.get('file_path')
        if file_path:
            usage_dict['files_read'].append({
                'path': normalize_path(file_path),
                'operation': 'read'
            })
    
    # Handle Edit and MultiEdit tools
    elif tool_name in ['Edit', 'MultiEdit']:
        path = inputs.get('file_path')
        if path:
            usage_dict['files_edited'].append({
                'path': normalize_path(path),
                'operation': tool_name.lower()
            })
    
    # Handle Write tool
    elif tool_name == 'Write':
        path = inputs.get('file_path')
        if path:
            usage_dict['files_created'].append({
                'path': normalize_path(path),
                'operation': 'write'
            })
    
    # Handle Grep tool
    elif tool_name == 'Grep':
        pattern = inputs.get('pattern')
        path = inputs.get('path', '.')
        if pattern:
            usage_dict['grep_searches'].append({
                'pattern': pattern,
                'path': normalize_path(path)
            })
    
    # Handle Bash tool
    elif tool_name == 'Bash':
        command = inputs.get('command')
        if command:
            usage_dict['bash_commands'].append({
                'command': command[:200]  # Limit command length
            })
    
    # Handle Glob tool
    elif tool_name == 'Glob':
        pattern = inputs.get('pattern')
        if pattern:
            usage_dict['glob_patterns'].append({
                'pattern': pattern
            })
    
    # Handle Task tool
    elif tool_name == 'Task':
        agent = inputs.get('subagent_type', 'unknown')
        usage_dict['task_calls'].append({
            'agent': agent
        })
    
    # Handle WebSearch tool
    elif tool_name == 'WebSearch':
        query = inputs.get('query')
        if query:
            usage_dict['web_searches'].append({
                'query': query[:100]
            })

def extract_tools_from_text(content: str, usage_dict: Dict[str, Any]):
    """Extract tool usage from text content (fallback for legacy format)."""
    # Look for file paths that might have been read/edited
    file_pattern = r'(?:Reading|Editing|Writing|Checking)\s+(?:file\s+)?([/~][\w\-./]+\.\w+)'
    for match in re.finditer(file_pattern, content):
        file_path = match.group(1)
        if 'Edit' in match.group(0):
            usage_dict['files_edited'].append({
                'path': normalize_path(file_path),
                'operation': 'edit'
            })
        else:
            usage_dict['files_read'].append({
                'path': normalize_path(file_path),
                'operation': 'read'
            })

def load_state():
    """Load the delta update state."""
    state_path = Path(STATE_FILE)
    if state_path.exists():
        try:
            with open(state_path, 'r') as f:
                return json.load(f)
        except Exception as e:
            logger.warning(f"Failed to load state: {e}")
    
    return {
        "last_update": None,
        "updated_conversations": {}
    }

def save_state(state: Dict[str, Any]):
    """Save the delta update state."""
    state_path = Path(STATE_FILE)
    state_path.parent.mkdir(parents=True, exist_ok=True)
    
    try:
        with open(state_path, 'w') as f:
            json.dump(state, f, indent=2)
    except Exception as e:
        logger.error(f"Failed to save state: {e}")

def get_recent_conversations(days: int = 7) -> List[Path]:
    """Get conversation files from the past N days."""
    recent_files = []
    cutoff_time = datetime.now() - timedelta(days=days)
    
    logs_path = Path(LOGS_DIR)
    if not logs_path.exists():
        logger.error(f"Logs directory not found: {LOGS_DIR}")
        return recent_files
    
    # Find all JSONL files
    for jsonl_file in logs_path.glob("**/*.jsonl"):
        try:
            # Check file modification time
            mtime = datetime.fromtimestamp(jsonl_file.stat().st_mtime)
            if mtime >= cutoff_time:
                recent_files.append(jsonl_file)
        except Exception as e:
            logger.debug(f"Error checking file {jsonl_file}: {e}")
    
    logger.info(f"Found {len(recent_files)} conversations from the past {days} days")
    return recent_files

def update_point_metadata(conversation_id: str, chunk_index: int, metadata: Dict[str, Any], 
                         collection_name: str) -> bool:
    """Update metadata for a specific point in Qdrant."""
    try:
        # Calculate point ID (same as original import)
        point_id_str = hashlib.md5(
            f"{conversation_id}_{chunk_index}".encode()
        ).hexdigest()[:16]
        point_id = int(point_id_str, 16) % (2**63)
        
        if DRY_RUN:
            logger.info(f"[DRY RUN] Would update point {point_id} with metadata")
            return True
        
        # First, try to get the existing point to preserve other fields
        try:
            existing_points = client.retrieve(
                collection_name=collection_name,
                ids=[point_id],
                with_payload=True,
                with_vectors=False
            )
            
            if existing_points:
                # Merge with existing payload
                existing_payload = existing_points[0].payload
                existing_payload.update(metadata)
                metadata = existing_payload
        except Exception as e:
            logger.debug(f"Could not retrieve existing point {point_id}: {e}")
        
        # Use set_payload to update just the metadata without touching the vector
        client.set_payload(
            collection_name=collection_name,
            payload=metadata,
            points=[point_id],
            wait=False  # Don't wait for each point
        )
        
        return True
        
    except Exception as e:
        import traceback
        logger.error(f"Failed to update point {conversation_id}_{chunk_index}: {e}")
        logger.debug(traceback.format_exc())
        return False

def process_conversation(jsonl_file: Path, state: Dict[str, Any]) -> bool:
    """Process a single conversation file and update its metadata."""
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
        
        logger.info(f"Processing: {conversation_id}")
        
        # Extract tool usage metadata
        tool_usage = extract_tool_usage_from_jsonl(str(jsonl_file))
        
        # Read the full conversation to get text for concept extraction
        conversation_text = ""
        with open(jsonl_file, 'r', encoding='utf-8') as f:
            for line in f:
                if line.strip():
                    try:
                        data = json.loads(line)
                        if 'message' in data and data['message']:
                            msg = data['message']
                            if msg.get('content'):
                                if isinstance(msg['content'], str):
                                    conversation_text += msg['content'] + "\n"
                                elif isinstance(msg['content'], list):
                                    for item in msg['content']:
                                        if isinstance(item, dict) and item.get('text'):
                                            conversation_text += item['text'] + "\n"
                    except:
                        continue
        
        # Extract concepts
        concepts = extract_concepts(conversation_text[:10000], tool_usage)  # Limit text for concept extraction
        
        # Prepare metadata update
        files_analyzed = list(set([
            item['path'] if isinstance(item, dict) else item
            for item in tool_usage.get('files_read', [])
            if (isinstance(item, dict) and item.get('path')) or isinstance(item, str)
        ]))[:20]  # Limit to 20 files
        
        files_edited = list(set([
            item['path'] if isinstance(item, dict) else item
            for item in tool_usage.get('files_edited', [])
            if (isinstance(item, dict) and item.get('path')) or isinstance(item, str)
        ]))[:10]  # Limit to 10 files
        
        metadata_update = {
            "files_analyzed": files_analyzed,
            "files_edited": files_edited,
            "tools_used": list(tool_usage.get('tools_summary', {}).keys())[:20],
            "tool_summary": dict(list(tool_usage.get('tools_summary', {}).items())[:10]),
            "concepts": list(concepts)[:15],
            "search_patterns": [s.get('pattern', '') for s in tool_usage.get('grep_searches', [])][:10],
            "analysis_only": len(files_edited) == 0 and len(tool_usage.get('files_created', [])) == 0,
            "has_file_metadata": True,  # Flag to indicate this has been enhanced
            "metadata_updated_at": datetime.now().isoformat()
        }
        
        # Determine collection name
        project_hash = hashlib.md5(normalize_project_name(project_name).encode()).hexdigest()[:8]
        collection_name = f"conv_{project_hash}{get_collection_suffix()}"
        
        # Check if collection exists
        try:
            collections = client.get_collections().collections
            if collection_name not in [c.name for c in collections]:
                logger.warning(f"Collection {collection_name} not found for project {project_name}")
                return False
        except Exception as e:
            logger.error(f"Error checking collection: {e}")
            return False
        
        # Get the number of chunks for this conversation
        # We need to know how many chunks were created during original import
        # For now, we'll try to update up to 50 chunks (most conversations have fewer)
        max_chunks = 50
        updated_count = 0
        failed_count = 0
        
        for chunk_index in range(max_chunks):
            success = update_point_metadata(
                conversation_id,
                chunk_index,
                metadata_update,
                collection_name
            )
            
            if success:
                updated_count += 1
            else:
                failed_count += 1
                # If we get too many failures in a row, the conversation probably has fewer chunks
                if failed_count > 5:
                    break
        
        if updated_count > 0:
            logger.info(f"Updated {updated_count} chunks for {conversation_id}")
            
            # Update state
            state["updated_conversations"][conversation_id] = {
                "updated_at": time.time(),
                "chunks_updated": updated_count,
                "project": project_name
            }
            
            return True
        else:
            logger.warning(f"No chunks updated for {conversation_id}")
            return False
        
    except Exception as e:
        logger.error(f"Failed to process {jsonl_file}: {e}")
        return False

def main():
    """Main delta update function."""
    logger.info("=== Starting Delta Metadata Update ===")
    logger.info(f"Configuration:")
    logger.info(f"  Qdrant URL: {QDRANT_URL}")
    logger.info(f"  Logs directory: {LOGS_DIR}")
    logger.info(f"  Days to update: {DAYS_TO_UPDATE}")
    logger.info(f"  Embedding type: {'local' if PREFER_LOCAL_EMBEDDINGS else 'voyage'}")
    logger.info(f"  Dry run: {DRY_RUN}")
    
    # Load state
    state = load_state()
    
    # Get recent conversations
    recent_files = get_recent_conversations(DAYS_TO_UPDATE)
    
    if not recent_files:
        logger.info("No recent conversations found to update")
        return
    
    # Limit for testing
    if os.getenv("LIMIT"):
        limit = int(os.getenv("LIMIT"))
        recent_files = recent_files[:limit]
        logger.info(f"Limited to {limit} files for testing")
    
    # Process each conversation
    success_count = 0
    failure_count = 0
    
    for i, jsonl_file in enumerate(recent_files, 1):
        logger.info(f"Processing {i}/{len(recent_files)}: {jsonl_file.name}")
        
        if process_conversation(jsonl_file, state):
            success_count += 1
        else:
            failure_count += 1
        
        # Save state periodically
        if i % 10 == 0:
            save_state(state)
    
    # Final state save
    state["last_update"] = datetime.now().isoformat()
    save_state(state)
    
    # Summary
    logger.info("=== Delta Update Complete ===")
    logger.info(f"Successfully updated: {success_count} conversations")
    logger.info(f"Failed: {failure_count} conversations")
    logger.info(f"Total conversations in state: {len(state['updated_conversations'])}")
    
    if DRY_RUN:
        logger.info("This was a DRY RUN - no actual updates were made")

if __name__ == "__main__":
    main()