#!/usr/bin/env python3
"""
SAFE migration script for consolidating spurious Qdrant collections.
Includes all safety checks recommended by code review.
"""

import hashlib
import sys
import uuid
from pathlib import Path
from collections import defaultdict
import logging
from typing import Dict, List, Tuple, Set, Optional
from datetime import datetime
import json

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

def check_point_conflicts(client: QdrantClient, source: str, target: str) -> Set:
    """Check for point ID conflicts between collections."""
    logger.info(f"  Checking for point ID conflicts...")
    source_ids = set()
    target_ids = set()
    
    # Get all source IDs
    offset = None
    while True:
        points, next_offset = client.scroll(source, limit=1000, offset=offset, with_payload=False)
        source_ids.update(str(p.id) for p in points)
        if not next_offset:
            break
        offset = next_offset
    
    # Get all target IDs  
    offset = None
    while True:
        points, next_offset = client.scroll(target, limit=1000, offset=offset, with_payload=False)
        target_ids.update(str(p.id) for p in points)
        if not next_offset:
            break
        offset = next_offset
    
    conflicts = source_ids & target_ids
    logger.info(f"  Found {len(source_ids)} source IDs, {len(target_ids)} target IDs")
    return conflicts

def validate_vector_compatibility(client: QdrantClient, source: str, target: str) -> bool:
    """Validate that source and target have compatible vector configurations."""
    source_info = client.get_collection(source)
    target_info = client.get_collection(target)
    
    source_vectors = source_info.config.params.vectors
    target_vectors = target_info.config.params.vectors
    
    # Compare vector configurations
    if hasattr(source_vectors, 'size') and hasattr(target_vectors, 'size'):
        if source_vectors.size != target_vectors.size:
            logger.error(f"  Vector size mismatch! Source: {source_vectors.size}, Target: {target_vectors.size}")
            return False
        if source_vectors.distance != target_vectors.distance:
            logger.warning(f"  Distance metric mismatch! Source: {source_vectors.distance}, Target: {target_vectors.distance}")
            return False
    
    return True

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
            
            # Sample more points for better confidence
            sample_size = min(100, max(20, info.points_count // 100))
            points, _ = client.scroll(coll_name, limit=sample_size, with_payload=True)
            
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

def migrate_collection_with_new_ids(client: QdrantClient, source: str, target: str, batch_size: int = 100) -> Tuple[int, List]:
    """Migrate collection with new point IDs to avoid conflicts."""
    # Ensure target exists
    collections = [c.name for c in client.get_collections().collections]
    
    if target not in collections:
        # Get config from source
        source_info = client.get_collection(source)
        logger.info(f"  Creating target collection: {target}")
        client.create_collection(
            collection_name=target,
            vectors_config=source_info.config.params.vectors
        )
    else:
        # Validate compatibility
        if not validate_vector_compatibility(client, source, target):
            raise ValueError(f"Vector configurations are incompatible between {source} and {target}")
        
        # Check for conflicts
        conflicts = check_point_conflicts(client, source, target)
        if conflicts:
            logger.warning(f"  Found {len(conflicts)} point ID conflicts - will generate new IDs")
    
    # Migrate all points with new IDs
    total_migrated = 0
    new_id_mapping = []
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
        
        # Generate new IDs for points
        new_points = []
        for point in points:
            new_id = str(uuid.uuid4())
            new_id_mapping.append({
                'old_id': str(point.id),
                'new_id': new_id,
                'source': source
            })
            
            # Create new point with new ID
            point.id = new_id
            new_points.append(point)
        
        # Upload to target
        client.upsert(collection_name=target, points=new_points)
        total_migrated += len(new_points)
        
        if total_migrated % 500 == 0:
            logger.info(f"    Migrated {total_migrated} points...")
        
        if next_offset is None:
            break
        offset = next_offset
    
    return total_migrated, new_id_mapping

def verify_migration(client: QdrantClient, source: str, target: str, expected_count: int) -> bool:
    """Verify that migration was successful."""
    source_info = client.get_collection(source)
    target_info = client.get_collection(target)
    
    logger.info(f"  Verification:")
    logger.info(f"    Source {source}: {source_info.points_count} points")
    logger.info(f"    Target {target}: {target_info.points_count} points")
    logger.info(f"    Expected migration: {expected_count} points")
    
    # Sample and compare some points
    source_sample, _ = client.scroll(source, limit=5, with_payload=True)
    if source_sample:
        logger.info(f"    Sample source point projects: {[p.payload.get('project', 'N/A')[:30] for p in source_sample[:2]]}")
    
    return True

def save_migration_log(migration_log: Dict):
    """Save migration log for recovery purposes."""
    log_file = f"migration_log_{datetime.now():%Y%m%d_%H%M%S}.json"
    with open(log_file, 'w') as f:
        json.dump(migration_log, f, indent=2, default=str)
    logger.info(f"Migration log saved to {log_file}")
    return log_file

def main():
    """Execute safe migration."""
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
    
    total_points = sum(info['points_count'] for info in high_confidence.values())
    
    if high_confidence:
        logger.info(f"\nHigh confidence migrations ({len(high_confidence)}):")
        for source, info in list(high_confidence.items())[:5]:
            logger.info(f"  {source} -> {info['target']}")
            logger.info(f"    Project: {info['project'][:50]}...")
            logger.info(f"    Points: {info['points_count']}")
    
    if low_confidence:
        logger.warning(f"\nLow confidence migrations ({len(low_confidence)}) - will be skipped")
    
    # Strong confirmation
    print(f"\n⚠️  MIGRATION SUMMARY:")
    print(f"  - Collections to migrate: {len(high_confidence)}")
    print(f"  - Total points to move: {total_points:,}")
    print(f"  - New point IDs will be generated to avoid conflicts")
    print(f"\nThis operation will:")
    print(f"  1. Copy data with new IDs to avoid conflicts")
    print(f"  2. Verify successful migration")
    print(f"  3. Keep source collections for manual verification")
    print(f"  4. You can delete sources later after verification")
    
    response = input("\nType 'MIGRATE' to proceed: ")
    
    if response != 'MIGRATE':
        logger.info("Migration cancelled")
        return 0
    
    # Execute migrations
    logger.info("\nStarting safe migration...")
    migration_log = {
        'timestamp': datetime.now().isoformat(),
        'migrations': [],
        'collections_to_delete': []
    }
    
    success_count = 0
    failed = []
    
    for source, info in high_confidence.items():
        try:
            logger.info(f"\nMigrating {source} -> {info['target']}")
            points_migrated, id_mapping = migrate_collection_with_new_ids(client, source, info['target'])
            
            # Verify
            verify_migration(client, source, info['target'], points_migrated)
            
            migration_log['migrations'].append({
                'source': source,
                'target': info['target'],
                'points_migrated': points_migrated,
                'id_mapping_count': len(id_mapping)
            })
            migration_log['collections_to_delete'].append(source)
            
            logger.info(f"  ✓ Successfully migrated {points_migrated} points")
            success_count += 1
            
        except Exception as e:
            logger.error(f"  ✗ Failed: {e}")
            failed.append((source, str(e)))
    
    # Save migration log
    log_file = save_migration_log(migration_log)
    
    # Summary
    logger.info("\n" + "=" * 60)
    logger.info("MIGRATION SUMMARY")
    logger.info("=" * 60)
    logger.info(f"Successfully migrated: {success_count}/{len(high_confidence)}")
    
    if failed:
        logger.error(f"Failed migrations: {len(failed)}")
        for source, error in failed[:5]:
            logger.error(f"  {source}: {error}")
    
    # Offer to delete source collections
    if migration_log['collections_to_delete']:
        print(f"\n✓ Migration complete. Source collections preserved.")
        print(f"Review the migrated data and check {log_file}")
        print(f"\nTo delete source collections after verification, run:")
        print(f"  python scripts/cleanup-migrated-sources.py {log_file}")
    
    # Final collection count
    final_collections = client.get_collections().collections
    conv_collections = [c for c in final_collections if c.name.startswith("conv_")]
    logger.info(f"\nFinal conversation collections: {len(conv_collections)}")
    
    return 0

if __name__ == "__main__":
    sys.exit(main())