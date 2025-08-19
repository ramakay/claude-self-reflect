#!/usr/bin/env python3
"""
Force metadata recovery script for Claude Self-Reflect.
Fixes conversations that were marked as updated but don't actually have metadata.
This addresses the point ID mismatch bug in delta-metadata-update.py.
"""

import os
import sys
import json
import hashlib
import re
import asyncio
from datetime import datetime
from typing import List, Dict, Any, Set, Optional
import logging
from pathlib import Path

from qdrant_client import QdrantClient

# Configuration
QDRANT_URL = os.getenv("QDRANT_URL", "http://localhost:6333")
LOGS_DIR = os.getenv("LOGS_DIR", os.path.expanduser("~/.claude/projects"))
DRY_RUN = os.getenv("DRY_RUN", "false").lower() == "true"
BATCH_SIZE = int(os.getenv("BATCH_SIZE", "100"))

# Set up logging
logging.basicConfig(level=logging.INFO, format='%(asctime)s - %(levelname)s - %(message)s')
logger = logging.getLogger(__name__)

# Initialize Qdrant client
client = QdrantClient(url=QDRANT_URL, timeout=30)

def normalize_path(path: str) -> str:
    """Normalize file paths for consistency."""
    if not path:
        return ""
    path = path.replace("/Users/", "~/").replace("\\Users\\", "~\\")
    path = path.replace("\\", "/")
    path = re.sub(r'/+', '/', path)
    return path

def extract_concepts(text: str) -> Set[str]:
    """Extract high-level concepts from text."""
    concepts = set()
    
    concept_patterns = {
        'freightwise': r'(freightwise|freight\s*wise)',
        'security': r'(security|vulnerability|CVE|injection|auth)',
        'docker': r'(docker|container|compose|kubernetes)',
        'testing': r'(test|pytest|unittest|coverage)',
        'api': r'(API|REST|GraphQL|endpoint)',
        'database': r'(database|SQL|query|migration|qdrant)',
        'debugging': r'(debug|error|exception|traceback)',
        'git': r'(git|commit|branch|merge|pull request)',
        'mcp': r'(MCP|claude-self-reflect|tool|agent)',
        'embeddings': r'(embedding|vector|semantic|similarity)',
    }
    
    text_lower = text.lower()
    for concept, pattern in concept_patterns.items():
        if re.search(pattern, text_lower, re.IGNORECASE):
            concepts.add(concept)
    
    return concepts

def extract_metadata_from_jsonl(jsonl_path: str) -> Dict[str, Any]:
    """Extract metadata from a JSONL conversation file."""
    metadata = {
        "files_analyzed": [],
        "files_edited": [],
        "tools_used": set(),
        "concepts": set(),
        "text_sample": ""
    }
    
    try:
        with open(jsonl_path, 'r', encoding='utf-8') as f:
            line_count = 0
            for line in f:
                line_count += 1
                if line_count > 200:  # Limit processing
                    break
                    
                if not line.strip():
                    continue
                    
                try:
                    data = json.loads(line)
                    if 'message' in data and data['message']:
                        msg = data['message']
                        
                        # Extract text for concept analysis
                        if msg.get('content'):
                            if isinstance(msg['content'], str):
                                metadata['text_sample'] += msg['content'][:500] + "\n"
                            
                        # Extract tool usage
                        if msg.get('role') == 'assistant' and msg.get('content'):
                            content = msg['content']
                            if isinstance(content, list):
                                for item in content:
                                    if isinstance(item, dict) and item.get('type') == 'tool_use':
                                        tool_name = item.get('name', '')
                                        metadata['tools_used'].add(tool_name)
                                        
                                        inputs = item.get('input', {})
                                        
                                        if tool_name == 'Read' and 'file_path' in inputs:
                                            metadata['files_analyzed'].append(
                                                normalize_path(inputs['file_path'])
                                            )
                                        elif tool_name in ['Edit', 'Write'] and 'file_path' in inputs:
                                            metadata['files_edited'].append(
                                                normalize_path(inputs['file_path'])
                                            )
                                            
                except json.JSONDecodeError:
                    continue
                    
    except Exception as e:
        logger.error(f"Error reading {jsonl_path}: {e}")
    
    # Extract concepts from collected text
    if metadata['text_sample']:
        metadata['concepts'] = extract_concepts(metadata['text_sample'][:5000])
    
    # Convert sets to lists and limit
    metadata['tools_used'] = list(metadata['tools_used'])[:20]
    metadata['concepts'] = list(metadata['concepts'])[:15]
    metadata['files_analyzed'] = list(set(metadata['files_analyzed']))[:20]
    metadata['files_edited'] = list(set(metadata['files_edited']))[:10]
    
    del metadata['text_sample']  # Don't store in Qdrant
    
    return metadata

async def find_conversations_without_metadata(collection_name: str) -> List[str]:
    """Find all unique conversation IDs that don't have metadata."""
    conversations_without_metadata = set()
    
    offset = None
    total_checked = 0
    
    while True:
        points, next_offset = client.scroll(
            collection_name=collection_name,
            limit=BATCH_SIZE,
            offset=offset,
            with_payload=True,
            with_vectors=False
        )
        
        if not points:
            break
            
        for point in points:
            # Check if metadata is missing
            if not point.payload.get('concepts') or not point.payload.get('has_file_metadata'):
                conv_id = point.payload.get('conversation_id')
                if conv_id:
                    conversations_without_metadata.add(conv_id)
        
        total_checked += len(points)
        offset = next_offset
        
        if offset is None:
            break
    
    logger.info(f"  Checked {total_checked} points, found {len(conversations_without_metadata)} conversations without metadata")
    return list(conversations_without_metadata)

async def update_conversation_points(collection_name: str, conversation_id: str, metadata: Dict[str, Any]) -> int:
    """Update all points for a conversation with metadata."""
    updated_count = 0
    
    # Get all points in the collection
    offset = None
    while True:
        points, next_offset = client.scroll(
            collection_name=collection_name,
            limit=BATCH_SIZE,
            offset=offset,
            with_payload=True,
            with_vectors=False
        )
        
        if not points:
            break
        
        # Find and update points for this conversation
        for point in points:
            if point.payload.get('conversation_id') == conversation_id:
                if not DRY_RUN:
                    # Merge metadata with existing payload
                    updated_payload = {**point.payload, **metadata}
                    updated_payload['has_file_metadata'] = True
                    updated_payload['metadata_updated_at'] = datetime.now().isoformat()
                    
                    client.set_payload(
                        collection_name=collection_name,
                        payload=updated_payload,
                        points=[point.id],
                        wait=False
                    )
                
                updated_count += 1
        
        offset = next_offset
        if offset is None:
            break
    
    return updated_count

async def process_collection(collection_name: str):
    """Process a single collection to add missing metadata."""
    logger.info(f"\nProcessing collection: {collection_name}")
    
    # Find conversations without metadata
    conversations_without_metadata = await find_conversations_without_metadata(collection_name)
    
    if not conversations_without_metadata:
        logger.info(f"  ✓ All conversations have metadata")
        return 0
    
    logger.info(f"  Found {len(conversations_without_metadata)} conversations needing metadata")
    
    # Process each conversation
    success_count = 0
    failed_count = 0
    
    for conv_id in conversations_without_metadata[:10]:  # Limit for testing
        # Find the JSONL file
        jsonl_pattern = f"**/{conv_id}.jsonl"
        jsonl_files = list(Path(LOGS_DIR).glob(jsonl_pattern))
        
        if not jsonl_files:
            logger.warning(f"  Cannot find JSONL for {conv_id}")
            failed_count += 1
            continue
        
        jsonl_file = jsonl_files[0]
        logger.info(f"  Processing {conv_id}")
        
        # Extract metadata
        metadata = extract_metadata_from_jsonl(str(jsonl_file))
        
        if not metadata['concepts'] and not metadata['files_analyzed']:
            logger.warning(f"    No metadata extracted from {conv_id}")
            failed_count += 1
            continue
        
        # Update points
        updated_points = await update_conversation_points(collection_name, conv_id, metadata)
        
        if updated_points > 0:
            logger.info(f"    ✓ Updated {updated_points} points with {len(metadata['concepts'])} concepts")
            success_count += 1
        else:
            logger.warning(f"    No points updated for {conv_id}")
            failed_count += 1
    
    logger.info(f"  Collection complete: {success_count} fixed, {failed_count} failed")
    return success_count

async def main():
    """Main recovery process."""
    logger.info("=== Force Metadata Recovery ===")
    logger.info(f"Qdrant URL: {QDRANT_URL}")
    logger.info(f"Dry run: {DRY_RUN}")
    
    # Get all collections
    collections = client.get_collections().collections
    
    # Focus on collections with potential issues
    priority_collections = []
    other_collections = []
    
    for collection in collections:
        name = collection.name
        if 'freightwise' in name.lower() or 'metafora' in name.lower() or '51e51d47' in name:
            priority_collections.append(name)
        elif name.startswith('conv_'):
            other_collections.append(name)
    
    logger.info(f"Found {len(priority_collections)} priority collections")
    logger.info(f"Found {len(other_collections)} other collections")
    
    # Process priority collections first
    total_fixed = 0
    
    for collection_name in priority_collections:
        fixed = await process_collection(collection_name)
        total_fixed += fixed
    
    # Process a sample of other collections
    for collection_name in other_collections[:5]:
        fixed = await process_collection(collection_name)
        total_fixed += fixed
    
    logger.info(f"\n=== Recovery Complete ===")
    logger.info(f"Total conversations fixed: {total_fixed}")
    
    if DRY_RUN:
        logger.info("This was a DRY RUN - no actual updates were made")

if __name__ == "__main__":
    asyncio.run(main())