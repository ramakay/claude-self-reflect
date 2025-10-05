#!/usr/bin/env python3
"""
Migrate data from spurious collections to correct project collections.
This fixes the remaining collections created with wrong hashes.
"""

import hashlib
import sys
from pathlib import Path
from collections import defaultdict
import logging
from typing import Dict, List, Tuple

from qdrant_client import QdrantClient
from qdrant_client.models import Distance, VectorParams
from dotenv import load_dotenv
import os

sys.path.insert(0, str(Path(__file__).parent.parent))
from scripts.utils import normalize_project_name

load_dotenv()

logging.basicConfig(level=logging.INFO, format='%(asctime)s - %(levelname)s - %(message)s')
logger = logging.getLogger(__name__)

QDRANT_URL = os.getenv("QDRANT_URL", "http://localhost:6333")
CLAUDE_PROJECTS_DIR = Path.home() / ".claude" / "projects"

def get_valid_project_hashes() -> Dict[str, str]:
    """Get all valid project hashes."""
    valid_hashes = {}
    for project_dir in CLAUDE_PROJECTS_DIR.iterdir():
        if project_dir.is_dir():
            normalized = normalize_project_name(project_dir.name)
            project_hash = hashlib.md5(normalized.encode()).hexdigest()[:8]
            valid_hashes[project_hash] = project_dir.name
    return valid_hashes

def analyze_spurious_collections(client: QdrantClient, valid_hashes: Dict[str, str]) -> Dict[str, Dict]:
    """Analyze all spurious collections to determine migration targets."""
    collections = client.get_collections().collections
    conv_colls = [c.name for c in collections if c.name.startswith("conv_")]
    
    migration_plan = {}
    
    for coll_name in conv_colls:
        parts = coll_name.split("_")
        if len(parts) >= 3:
            coll_hash = parts[1]
            
            # Skip valid collections
            if coll_hash in valid_hashes:
                continue
            
            # Check if has data
            info = client.get_collection(coll_name)
            if info.points_count == 0:
                continue
            
            # Sample points to determine project
            points, _ = client.scroll(coll_name, limit=20, with_payload=True)
            
            project_counts = defaultdict(int)
            for point in points:
                # Check both 'project' and 'project_name' fields
                project = point.payload.get('project') or point.payload.get('project_name')
                if project:
                    project_counts[project] += 1
            
            if project_counts:
                # Get most common project
                most_common = max(project_counts.items(), key=lambda x: x[1])
                project_name = most_common[0]
                confidence = most_common[1] / len(points)
                
                # Calculate correct collection name
                normalized = normalize_project_name(project_name)
                correct_hash = hashlib.md5(normalized.encode()).hexdigest()[:8]
                suffix = "_local" if coll_name.endswith("_local") else "_voyage"
                target_collection = f"conv_{correct_hash}{suffix}"
                
                # Only add to migration plan if target is different from source
                if target_collection != coll_name:
                    migration_plan[coll_name] = {
                        'target': target_collection,
                        'project': project_name,
                        'normalized': normalized,
                        'points_count': info.points_count,
                        'confidence': confidence
                    }
                else:
                    logger.debug(f"Skipping {coll_name} - already in correct collection")
    
    return migration_plan

def migrate_collection(client: QdrantClient, source: str, target: str, batch_size: int = 100) -> int:
    """Migrate all points from source to target collection."""
    # Ensure target exists
    collections = [c.name for c in client.get_collections().collections]
    
    if target not in collections:
        # Get config from source
        source_info = client.get_collection(source)
        logger.info(f"Creating target collection: {target}")
        client.create_collection(
            collection_name=target,
            vectors_config=source_info.config.params.vectors
        )
    
    # Migrate all points
    total_migrated = 0
    offset = None
    
    while True:
        points, next_offset = client.scroll(
            collection_name=source,
            limit=batch_size,
            offset=offset,
            with_payload=True,
            with_vectors=True
        )
        
        if not points:
            break
        
        # Upload to target
        client.upsert(collection_name=target, points=points)
        total_migrated += len(points)
        
        if total_migrated % 500 == 0:
            logger.info(f"  Migrated {total_migrated} points...")
        
        if next_offset is None:
            break
        offset = next_offset
    
    return total_migrated

def main():
    """Execute migration."""
    client = QdrantClient(url=QDRANT_URL)
    
    # Get valid hashes
    valid_hashes = get_valid_project_hashes()
    logger.info(f"Found {len(valid_hashes)} valid projects")
    
    # Analyze spurious collections
    logger.info("Analyzing spurious collections...")
    migration_plan = analyze_spurious_collections(client, valid_hashes)
    
    if not migration_plan:
        logger.info("No collections need migration")
        return 0
    
    logger.info(f"\nFound {len(migration_plan)} collections to migrate:")
    
    # Group by confidence
    high_confidence = {k: v for k, v in migration_plan.items() if v['confidence'] >= 0.95}
    low_confidence = {k: v for k, v in migration_plan.items() if v['confidence'] < 0.95}
    
    if high_confidence:
        logger.info(f"\nHigh confidence migrations ({len(high_confidence)}):")
        for source, info in list(high_confidence.items())[:5]:
            logger.info(f"  {source} -> {info['target']}")
            logger.info(f"    Project: {info['project'][:50]}...")
            logger.info(f"    Points: {info['points_count']}")
    
    if low_confidence:
        logger.warning(f"\nLow confidence migrations ({len(low_confidence)}):")
        for source, info in list(low_confidence.items())[:3]:
            logger.warning(f"  {source}: confidence={info['confidence']:.2f}")
    
    # Ask for confirmation
    response = input(f"\nMigrate {len(high_confidence)} high-confidence collections? (yes/no): ")
    
    if response.lower() != 'yes':
        logger.info("Migration cancelled")
        return 0
    
    # Execute migrations
    logger.info("\nStarting migration...")
    success_count = 0
    failed = []
    
    for source, info in high_confidence.items():
        try:
            logger.info(f"\nMigrating {source} -> {info['target']}")
            points_migrated = migrate_collection(client, source, info['target'])
            logger.info(f"  ✓ Migrated {points_migrated} points")
            
            # Delete source collection
            client.delete_collection(source)
            logger.info(f"  ✓ Deleted source collection")
            success_count += 1
            
        except Exception as e:
            logger.error(f"  ✗ Failed: {e}")
            failed.append((source, str(e)))
    
    # Summary
    logger.info("\n" + "=" * 60)
    logger.info("MIGRATION SUMMARY")
    logger.info("=" * 60)
    logger.info(f"Successfully migrated: {success_count}/{len(high_confidence)}")
    
    if failed:
        logger.error(f"Failed migrations: {len(failed)}")
        for source, error in failed[:5]:
            logger.error(f"  {source}: {error}")
    
    # Final collection count
    final_collections = client.get_collections().collections
    conv_collections = [c for c in final_collections if c.name.startswith("conv_")]
    logger.info(f"\nFinal conversation collections: {len(conv_collections)}")
    
    return 0

if __name__ == "__main__":
    sys.exit(main())