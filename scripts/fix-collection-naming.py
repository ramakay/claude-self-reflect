#!/usr/bin/env python3
"""
Migration script to fix orphaned collections created by Docker path normalization bug.
This merges wrongly-named collections (both _local and _voyage) into correctly-named ones.

The bug: Docker containers created collections with hash of full path instead of project name.
Example: conv_22f17df6_* (wrong) should be conv_7f6df0fc_* (correct) for claude-self-reflect

Usage:
    # Dry run (default) - shows what would be migrated
    python fix-collection-naming.py
    
    # Execute migrations
    python fix-collection-naming.py --execute
"""

import sys
import hashlib
import logging
import argparse
from pathlib import Path
from qdrant_client import QdrantClient
from qdrant_client.models import PointStruct

# Add parent directory for imports
sys.path.insert(0, str(Path(__file__).parent.parent))
from scripts.utils import normalize_project_name

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

def migrate_collections(dry_run=True):
    """Migrate points from wrong collections to correct ones."""
    client = QdrantClient(url="http://localhost:6333")
    
    # Get all collections
    collections = client.get_collections().collections
    collection_names = [c.name for c in collections]
    
    # Known wrong -> correct mappings
    migrations = [
        # claude-self-reflect project
        ("conv_22f17df6_local", "conv_7f6df0fc_local"),
        ("conv_22f17df6_voyage", "conv_7f6df0fc_voyage"),
    ]
    
    # Add detection for other potentially affected projects
    logger.info(f"Checking {len(collection_names)} collections for migration needs...")
    
    # Process each migration
    total_migrated = 0
    for source_collection, target_collection in migrations:
        if source_collection not in collection_names:
            continue
            
        # Check if we need to migrate
        source_info = client.get_collection(source_collection)
        if source_info.points_count == 0:
            logger.info(f"Skipping empty collection: {source_collection}")
            continue
            
        logger.info(f"\n{'=' * 60}")
        logger.info(f"Migration: {source_collection} -> {target_collection}")
        logger.info(f"Points to migrate: {source_info.points_count}")
        
        if dry_run:
            logger.info("[DRY RUN] Would migrate this collection")
            total_migrated += source_info.points_count
            continue
        
        try:
            # Get all points from source
            all_points = []
            offset = None
            
            while True:
                batch, offset = client.scroll(
                    collection_name=source_collection,
                    limit=100,
                    offset=offset,
                    with_payload=True,
                    with_vectors=True
                )
                
                if not batch:
                    break
                    
                all_points.extend(batch)
                if offset is None:
                    break
            
            if not all_points:
                logger.info("No points to migrate")
                continue
            
            # Ensure target collection exists
            if target_collection not in collection_names:
                # Get source collection config
                source_config = source_info.config
                
                # Create target with same config
                client.recreate_collection(
                    collection_name=target_collection,
                    vectors_config=source_config.params.vectors,
                    on_disk_payload=source_config.params.on_disk_payload
                )
                logger.info(f"Created target collection: {target_collection}")
            
            # Migrate points in batches
            batch_size = 50
            for i in range(0, len(all_points), batch_size):
                batch = all_points[i:i+batch_size]
                
                # Fix project names in payload and convert to PointStruct
                points_to_insert = []
                for point in batch:
                    payload = dict(point.payload) if point.payload else {}
                    if 'project' in payload:
                        # Normalize the project name
                        payload['project'] = normalize_project_name(payload['project'])
                    
                    points_to_insert.append(PointStruct(
                        id=point.id,
                        vector=point.vector,
                        payload=payload
                    ))
                
                # Upsert to target
                client.upsert(
                    collection_name=target_collection,
                    points=points_to_insert
                )
                
                logger.info(f"Migrated batch {i//batch_size + 1}/{(len(all_points) + batch_size - 1)//batch_size}")
            
            logger.info(f"✅ Successfully migrated {len(all_points)} points")
            total_migrated += len(all_points)
            
            # Delete source collection after successful migration
            client.delete_collection(source_collection)
            logger.info(f"Deleted source collection: {source_collection}")
            
        except Exception as e:
            logger.error(f"Migration failed: {e}")
            raise
    
    logger.info(f"\n{'=' * 60}")
    if dry_run:
        logger.info(f"DRY RUN COMPLETE: Would migrate {total_migrated} points total")
        logger.info("Run with --execute to perform actual migration")
    else:
        logger.info(f"MIGRATION COMPLETE: Migrated {total_migrated} points total")
    
    client.close()


def main():
    parser = argparse.ArgumentParser(
        description="Fix orphaned collections from Docker path normalization bug"
    )
    parser.add_argument(
        '--execute',
        action='store_true',
        help='Actually perform migrations (default is dry-run)'
    )
    
    args = parser.parse_args()
    migrate_collections(dry_run=not args.execute)


if __name__ == "__main__":
    main()