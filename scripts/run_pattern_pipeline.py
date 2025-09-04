#!/usr/bin/env python3
"""
Complete pattern pipeline: Extract patterns from conversations then propagate them
"""

import os
import sys
import logging
from pathlib import Path
from datetime import datetime, timedelta, timezone
from qdrant_client import QdrantClient

# Add parent directory to path for imports
sys.path.append(str(Path(__file__).parent))

from ast_pattern_extractor import process_collection_patterns
from pattern_propagator import PatternPropagator

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

def run_pattern_pipeline(days_back=7, limit_collections=10):
    """
    Run the complete pattern pipeline:
    1. Extract patterns from recent conversations
    2. Propagate patterns across chunks
    """
    
    client = QdrantClient(
        url=os.getenv("QDRANT_URL", "http://localhost:6333"),
        api_key=os.getenv("QDRANT_API_KEY")
    )
    
    # Get all collections
    collections = client.get_collections().collections
    logger.info(f"Found {len(collections)} total collections")
    
    # Filter for recent collections (if they have timestamps in metadata)
    recent_collections = []
    cutoff_date = datetime.now(timezone.utc) - timedelta(days=days_back)
    
    for collection in collections[:limit_collections]:
        try:
            # Check if collection has any recent points
            info = client.get_collection(collection.name)
            if info.points_count > 0:
                recent_collections.append(collection.name)
        except Exception as e:
            logger.debug(f"Error checking {collection.name}: {e}")
    
    logger.info(f"Processing {len(recent_collections)} collections")
    
    # Track statistics
    total_extracted = 0
    total_propagated = 0
    
    for i, collection_name in enumerate(recent_collections):
        logger.info(f"\n{'='*60}")
        logger.info(f"Processing collection {i+1}/{len(recent_collections)}: {collection_name}")
        logger.info(f"{'='*60}")
        
        try:
            # Step 1: Extract patterns using AST
            logger.info("Step 1: Extracting AST patterns...")
            extracted = process_collection_patterns(
                collection_name=collection_name,
                project_filter=None,
                limit=None
            )
            
            if extracted and 'points_with_patterns' in extracted:
                total_extracted += extracted['points_with_patterns']
                logger.info(f"  ✓ Extracted patterns from {extracted['points_with_patterns']} points")
                logger.info(f"    Pattern categories found: {extracted.get('unique_patterns', {})}")
            else:
                logger.info(f"  - No patterns extracted")
            
            # Step 2: Propagate patterns across chunks
            logger.info("Step 2: Propagating patterns across chunks...")
            propagator = PatternPropagator(client)
            
            # Get all unique conversations in the collection
            conversations = set()
            offset = None
            while True:
                result = client.scroll(
                    collection_name=collection_name,
                    limit=100,
                    offset=offset,
                    with_payload=['conversation_id'],
                    with_vectors=False
                )
                points, next_offset = result
                if not points:
                    break
                for point in points:
                    conv_id = point.payload.get('conversation_id')
                    if conv_id:
                        conversations.add(conv_id)
                offset = next_offset
                if offset is None:
                    break
            
            logger.info(f"  Found {len(conversations)} unique conversations")
            
            # Propagate patterns for each conversation
            collection_propagated = 0
            for conv_id in conversations:
                stats = propagator.propagate_patterns_weighted(collection_name, conv_id)
                collection_propagated += stats['propagated_to']
            
            total_propagated += collection_propagated
            logger.info(f"  ✓ Propagated patterns to {collection_propagated} chunks")
            
        except Exception as e:
            logger.error(f"Error processing {collection_name}: {e}")
            continue
    
    # Final summary
    logger.info(f"\n{'='*60}")
    logger.info(f"PATTERN PIPELINE COMPLETE")
    logger.info(f"{'='*60}")
    logger.info(f"Collections processed: {len(recent_collections)}")
    logger.info(f"Total patterns extracted: {total_extracted}")
    logger.info(f"Total chunks with propagated patterns: {total_propagated}")
    
    return {
        'collections': len(recent_collections),
        'extracted': total_extracted,
        'propagated': total_propagated
    }

if __name__ == "__main__":
    import argparse
    
    parser = argparse.ArgumentParser(description="Run complete pattern pipeline")
    parser.add_argument("--days", type=int, default=7, help="Process collections from last N days")
    parser.add_argument("--limit", type=int, default=10, help="Limit number of collections to process")
    
    args = parser.parse_args()
    
    run_pattern_pipeline(days_back=args.days, limit_collections=args.limit)