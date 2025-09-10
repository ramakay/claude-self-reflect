#!/usr/bin/env python3
"""
Backup orphaned collections to JSON and clean up Qdrant to pristine state.
Saves all points with their vectors and metadata for potential future recovery.
"""

import json
import logging
from datetime import datetime
from pathlib import Path
from qdrant_client import QdrantClient
import hashlib

logging.basicConfig(level=logging.INFO, format='%(asctime)s - %(levelname)s - %(message)s')
logger = logging.getLogger(__name__)


def backup_and_cleanup():
    """Backup orphaned collections and restore pristine state"""
    client = QdrantClient(url="http://localhost:6333")
    
    # Create backups directory
    backup_dir = Path("backups")
    backup_dir.mkdir(exist_ok=True)
    
    # Collections to backup and remove
    orphaned_collections = [
        "conv_22f17df6_voyage",  # Wrong hash, actually uses FastEmbed
        "conv_22f17df6_local",   # If it exists
    ]
    
    timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
    
    for collection_name in orphaned_collections:
        try:
            # Check if collection exists
            info = client.get_collection(collection_name)
            logger.info(f"\nProcessing {collection_name}:")
            logger.info(f"  Points: {info.points_count}")
            logger.info(f"  Vector dimensions: {info.config.params.vectors.size}")
            
            if info.points_count == 0:
                logger.info(f"  Skipping backup - collection is empty")
                client.delete_collection(collection_name)
                logger.info(f"  ✅ Deleted empty collection")
                continue
            
            # Backup all points
            backup_file = backup_dir / f"{collection_name}_{timestamp}.json"
            logger.info(f"  Backing up to: {backup_file}")
            
            all_points = []
            offset = None
            
            while True:
                batch, offset = client.scroll(
                    collection_name=collection_name,
                    limit=100,
                    offset=offset,
                    with_payload=True,
                    with_vectors=True
                )
                
                if not batch:
                    break
                
                # Convert points to serializable format
                for point in batch:
                    all_points.append({
                        'id': point.id,
                        'vector': point.vector,
                        'payload': dict(point.payload) if point.payload else {}
                    })
                
                if offset is None:
                    break
            
            # Save backup
            backup_data = {
                'collection_name': collection_name,
                'timestamp': timestamp,
                'points_count': len(all_points),
                'vector_size': info.config.params.vectors.size,
                'distance': info.config.params.vectors.distance.value,
                'points': all_points
            }
            
            with open(backup_file, 'w') as f:
                json.dump(backup_data, f, indent=2)
            
            logger.info(f"  ✅ Backed up {len(all_points)} points")
            
            # Delete the collection
            client.delete_collection(collection_name)
            logger.info(f"  ✅ Deleted collection {collection_name}")
            
        except Exception as e:
            if "doesn't exist" in str(e):
                logger.info(f"{collection_name}: Does not exist (already clean)")
            else:
                logger.error(f"Error processing {collection_name}: {e}")
    
    # Verify correct collections
    logger.info("\n" + "="*60)
    logger.info("VERIFICATION - Correct collections status:")
    
    correct_collections = {
        "conv_7f6df0fc_local": "claude-self-reflect (FastEmbed)",
        "conv_7f6df0fc_voyage": "claude-self-reflect (Voyage AI)",
    }
    
    for coll_name, description in correct_collections.items():
        try:
            info = client.get_collection(coll_name)
            logger.info(f"✅ {coll_name}: {info.points_count} points, {info.config.params.vectors.size} dims - {description}")
        except:
            logger.warning(f"⚠️  {coll_name}: Does not exist - {description}")
    
    # Check for any remaining wrong collections
    logger.info("\n" + "="*60)
    logger.info("FINAL CHECK - Looking for any remaining orphaned collections:")
    
    all_collections = client.get_collections().collections
    wrong_hash = "22f17df6"
    remaining_orphans = [c.name for c in all_collections if wrong_hash in c.name]
    
    if remaining_orphans:
        logger.warning(f"⚠️  Found remaining orphaned collections: {remaining_orphans}")
        logger.warning("Run this script again to clean them up")
    else:
        logger.info("✅ No orphaned collections found - system is pristine!")
    
    # Summary
    logger.info("\n" + "="*60)
    logger.info("SUMMARY:")
    logger.info(f"- Backups saved to: {backup_dir.absolute()}")
    logger.info(f"- Total collections in Qdrant: {len(all_collections)}")
    logger.info("- System restored to pristine state")
    
    client.close()


if __name__ == "__main__":
    backup_and_cleanup()