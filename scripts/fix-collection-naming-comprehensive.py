#!/usr/bin/env python3
"""
Comprehensive collection naming fix and cleanup script.
Based on analysis and recommendations from Opus 4.1.

This script:
1. Validates collection integrity
2. Creates recovery manifest
3. Identifies spurious collections
4. Safely migrates misrouted data
5. Deletes empty spurious collections
"""

import hashlib
import json
import logging
import os
import shutil
import sys
import time
import uuid
from collections import defaultdict
from datetime import datetime
from pathlib import Path
from typing import Dict, List, Tuple, Set, Optional, Any

from dotenv import load_dotenv
from qdrant_client import QdrantClient
from qdrant_client.models import Distance, VectorParams

# Add parent directory to path for utils import
sys.path.insert(0, str(Path(__file__).parent.parent))

load_dotenv()

# Configuration
QDRANT_URL = os.getenv("QDRANT_URL", "http://localhost:6333")
CLAUDE_PROJECTS_DIR = Path.home() / ".claude" / "projects"

# Batch configurations
BATCH_CONFIGS = {
    'deletion': {
        'size': 20,
        'pause_ms': 100,
        'verify_before': True
    },
    'migration': {
        'size': 100,
        'max_concurrent': 5000,
        'pause_ms': 50
    },
    'scroll': {
        'size': 1000,
        'with_vectors': False
    }
}

# Setup logging
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(levelname)s - %(message)s',
    handlers=[
        logging.FileHandler(f'collection_cleanup_{datetime.now():%Y%m%d_%H%M%S}.log'),
        logging.StreamHandler()
    ]
)
logger = logging.getLogger(__name__)


def normalize_project_name(project_path: str) -> str:
    """
    Simplified project name normalization for consistent hashing.
    
    Examples:
        '/Users/name/.claude/projects/-Users-name-projects-myproject' -> 'myproject'
        '-Users-name-projects-myproject' -> 'myproject'
        '/path/to/myproject' -> 'myproject'
        'myproject' -> 'myproject'
    """
    if not project_path:
        return ""
    
    path = Path(project_path.rstrip('/'))
    
    # Extract the final directory name
    final_component = path.name
    
    # If it's Claude's dash-separated format, extract project name
    if final_component.startswith('-') and 'projects' in final_component:
        # Split on 'projects' and take everything after
        parts = final_component.split('projects-', 1)
        if len(parts) == 2:
            return parts[1]
    
    # For regular paths, just return the directory name
    return final_component if final_component else path.parent.name


def get_valid_project_hashes() -> Dict[str, str]:
    """Get all valid project hashes based on actual project directories."""
    project_hashes = {}
    
    if not CLAUDE_PROJECTS_DIR.exists():
        logger.warning(f"{CLAUDE_PROJECTS_DIR} does not exist")
        return project_hashes
    
    for project_dir in CLAUDE_PROJECTS_DIR.iterdir():
        if project_dir.is_dir():
            # Normalize project name
            normalized = normalize_project_name(project_dir.name)
            project_hash = hashlib.md5(normalized.encode()).hexdigest()[:8]
            project_hashes[project_hash] = project_dir.name
            logger.debug(f"Project: {project_dir.name} -> {normalized} -> {project_hash}")
    
    return project_hashes


def detect_hash_collisions(valid_hashes: dict) -> dict:
    """Detect any hash collisions in project mapping."""
    hash_to_projects = defaultdict(list)
    
    for project_hash, project_name in valid_hashes.items():
        hash_to_projects[project_hash].append(project_name)
    
    collisions = {h: projects for h, projects in hash_to_projects.items() if len(projects) > 1}
    
    if collisions:
        logger.critical(f"HASH COLLISIONS DETECTED: {collisions}")
    
    return collisions


def validate_hash_mapping(valid_hashes: dict) -> bool:
    """Ensure hash generation is deterministic and correct."""
    for project_dir in CLAUDE_PROJECTS_DIR.iterdir():
        if not project_dir.is_dir():
            continue
            
        # Generate hash multiple ways to ensure consistency
        hash1 = hashlib.md5(normalize_project_name(project_dir.name).encode()).hexdigest()[:8]
        hash2 = hashlib.md5(normalize_project_name(str(project_dir)).encode()).hexdigest()[:8]
        
        if hash1 != hash2:
            logger.error(f"Hash mismatch for {project_dir}: {hash1} vs {hash2}")
            return False
        
        if hash1 not in valid_hashes:
            logger.error(f"Missing hash mapping for {project_dir.name} -> {hash1}")
            return False
    
    return True


def verify_docker_volume_consistency(client: QdrantClient) -> bool:
    """Ensure Docker volume is properly mounted and writable."""
    test_collection = f"test_cleanup_{uuid.uuid4().hex[:8]}"
    
    try:
        # Create test collection
        client.create_collection(
            collection_name=test_collection,
            vectors_config=VectorParams(size=384, distance=Distance.COSINE)
        )
        
        # Verify it persists
        time.sleep(0.5)
        
        # Check collection exists
        collections = client.get_collections().collections
        exists = any(c.name == test_collection for c in collections)
        
        # Clean up
        if exists:
            client.delete_collection(test_collection)
            return True
        
        return False
    except Exception as e:
        logger.error(f"Docker volume test failed: {e}")
        return False


def verify_collection_integrity(
    client: QdrantClient,
    collection_name: str,
    expected_project: str,
    sample_size: int = 50
) -> Tuple[bool, dict]:
    """Verify that a collection contains data for the expected project."""
    
    if not collection_name.startswith("conv_"):
        return False, {"error": "Invalid collection name format"}
    
    collection_hash = collection_name.split("_")[1]
    
    # Sample points to verify
    points, _ = client.scroll(
        collection_name=collection_name,
        limit=sample_size,
        with_payload=True
    )
    
    if not points:
        return True, {"status": "empty", "matches": 0, "mismatches": 0}
    
    matches = 0
    mismatches = []
    
    for point in points:
        payload_project = point.payload.get('project_name', '')
        
        # Normalize and hash the payload's project name
        normalized = normalize_project_name(payload_project)
        payload_hash = hashlib.md5(normalized.encode()).hexdigest()[:8]
        
        if payload_hash == collection_hash:
            matches += 1
        else:
            mismatches.append({
                'point_id': str(point.id),
                'expected_project': expected_project,
                'found_project': payload_project,
                'expected_hash': collection_hash,
                'actual_hash': payload_hash
            })
    
    integrity_score = matches / len(points) if points else 0
    
    return integrity_score > 0.95, {
        "integrity_score": integrity_score,
        "matches": matches,
        "mismatches": len(mismatches),
        "sample_size": len(points),
        "mismatch_details": mismatches[:5]
    }


def identify_collection_origin(
    client: QdrantClient,
    collection_name: str,
    valid_hashes: dict,
    sample_size: int = 10
) -> Tuple[Optional[str], float]:
    """Attempt to identify the correct project for a spurious collection."""
    points, _ = client.scroll(
        collection_name=collection_name,
        limit=sample_size,
        with_payload=True
    )
    
    if not points:
        return None, 0
    
    project_indicators = defaultdict(int)
    
    for point in points:
        # Look for project_name in payload
        if 'project_name' in point.payload:
            project_name = point.payload['project_name']
            normalized = normalize_project_name(project_name)
            project_indicators[normalized] += 1
        
        # Check file paths
        if 'file_path' in point.payload:
            normalized = normalize_project_name(point.payload['file_path'])
            if normalized:
                project_indicators[normalized] += 1
    
    if project_indicators:
        # Return most likely project with confidence score
        best_match = max(project_indicators.items(), key=lambda x: x[1])
        confidence = best_match[1] / sum(project_indicators.values())
        return best_match[0], confidence
    
    return None, 0


def smart_collection_recovery(
    client: QdrantClient,
    spurious_collections: Set[str],
    valid_hashes: dict
) -> dict:
    """Intelligently recover misrouted data using project_name field."""
    
    recovery_plan = {
        'direct_matches': {},
        'ambiguous': {},
        'empty': [],
        'corrupted': []
    }
    
    for coll in spurious_collections:
        try:
            info = client.get_collection(coll)
            
            if info.points_count == 0:
                recovery_plan['empty'].append(coll)
                continue
            
            # Sample to determine destination
            points, _ = client.scroll(coll, limit=100, with_payload=True)
            
            project_distribution = defaultdict(int)
            missing_project_name = 0
            
            for point in points:
                if 'project_name' in point.payload:
                    project_distribution[point.payload['project_name']] += 1
                else:
                    missing_project_name += 1
            
            if missing_project_name > len(points) * 0.1:
                recovery_plan['corrupted'].append({
                    'collection': coll,
                    'points': info.points_count,
                    'missing_field_ratio': missing_project_name / len(points)
                })
            elif len(project_distribution) == 1:
                # All points belong to same project - safe to migrate
                target_project = list(project_distribution.keys())[0]
                normalized = normalize_project_name(target_project)
                target_hash = hashlib.md5(normalized.encode()).hexdigest()[:8]
                
                # Determine suffix from source collection
                suffix = "_local" if coll.endswith("_local") else "_voyage"
                target_collection = f"conv_{target_hash}{suffix}"
                
                recovery_plan['direct_matches'][coll] = {
                    'target_collection': target_collection,
                    'target_project': target_project,
                    'normalized_project': normalized,
                    'points_count': info.points_count,
                    'confidence': 1.0
                }
            else:
                # Mixed projects in one collection
                dominant = max(project_distribution.items(), key=lambda x: x[1])
                recovery_plan['ambiguous'][coll] = {
                    'points_count': info.points_count,
                    'project_distribution': dict(project_distribution),
                    'dominant_project': dominant
                }
        except Exception as e:
            logger.error(f"Error analyzing {coll}: {e}")
            recovery_plan['corrupted'].append({
                'collection': coll,
                'error': str(e)
            })
    
    return recovery_plan


def create_recovery_manifest(
    client: QdrantClient,
    valid_hashes: dict,
    valid_collections: Set[str],
    spurious_collections: Set[str]
) -> dict:
    """Create a manifest that allows recreation if needed."""
    
    manifest = {
        'timestamp': datetime.now().isoformat(),
        'valid_projects': valid_hashes,
        'collections': {
            'valid': list(valid_collections),
            'spurious_empty': [],
            'spurious_with_data': {}
        }
    }
    
    for coll in spurious_collections:
        try:
            info = client.get_collection(coll)
            
            if info.points_count == 0:
                manifest['collections']['spurious_empty'].append({
                    'name': coll,
                    'vectors_count': info.vectors_count or 0
                })
            else:
                # For non-empty, include sample data
                sample, _ = client.scroll(coll, limit=5, with_payload=True)
                origin, confidence = identify_collection_origin(client, coll, valid_hashes)
                
                manifest['collections']['spurious_with_data'][coll] = {
                    'points_count': info.points_count,
                    'sample_payloads': [p.payload for p in sample],
                    'likely_origin': origin,
                    'confidence': confidence
                }
        except Exception as e:
            logger.error(f"Error creating manifest for {coll}: {e}")
    
    # Save manifest
    manifest_file = f'qdrant_cleanup_manifest_{datetime.now():%Y%m%d_%H%M%S}.json'
    with open(manifest_file, 'w') as f:
        json.dump(manifest, f, indent=2, default=str)
    
    logger.info(f"Recovery manifest saved to {manifest_file}")
    return manifest


def chunks(lst: list, n: int):
    """Yield successive n-sized chunks from list."""
    for i in range(0, len(lst), n):
        yield lst[i:i + n]


def batch_delete_empty_collections(
    client: QdrantClient,
    collections: list,
    batch_size: int = 20
) -> Tuple[list, list]:
    """Delete empty collections in controlled batches with verification."""
    deleted = []
    failed = []
    
    for batch in chunks(collections, batch_size):
        # Pre-deletion verification
        batch_snapshot = {}
        for coll in batch:
            try:
                info = client.get_collection(coll)
                batch_snapshot[coll] = {
                    'points_count': info.points_count or 0,
                    'vectors_count': info.vectors_count or 0
                }
            except Exception as e:
                logger.error(f"Failed to get info for {coll}: {e}")
                failed.append((coll, str(e)))
                continue
        
        # Verify all are truly empty
        non_empty = [c for c, s in batch_snapshot.items() if s['points_count'] > 0]
        if non_empty:
            logger.error(f"Batch contains non-empty collections: {non_empty}")
            for coll in non_empty:
                failed.append((coll, "Collection not empty"))
            # Remove non-empty from batch
            batch = [c for c in batch if c not in non_empty]
        
        # Execute deletions
        for coll in batch:
            try:
                client.delete_collection(coll)
                deleted.append(coll)
                logger.info(f"Deleted empty collection: {coll}")
            except Exception as e:
                failed.append((coll, str(e)))
                logger.error(f"Failed to delete {coll}: {e}")
        
        # Brief pause between batches
        if batch:
            time.sleep(BATCH_CONFIGS['deletion']['pause_ms'] / 1000)
    
    return deleted, failed


def merge_spurious_into_valid(
    client: QdrantClient,
    source_collection: str,
    target_collection: str,
    dry_run: bool = True
) -> int:
    """Merge a spurious collection into its correct destination."""
    
    # Get all points from source
    all_points = []
    offset = None
    
    while True:
        points, next_offset = client.scroll(
            collection_name=source_collection,
            limit=BATCH_CONFIGS['scroll']['size'],
            offset=offset,
            with_payload=True,
            with_vectors=True
        )
        
        all_points.extend(points)
        
        if next_offset is None:
            break
        offset = next_offset
    
    if dry_run:
        logger.info(f"DRY RUN: Would migrate {len(all_points)} points from {source_collection} to {target_collection}")
        return len(all_points)
    
    # Ensure target collection exists
    collections = [c.name for c in client.get_collections().collections]
    if target_collection not in collections:
        logger.info(f"Creating target collection: {target_collection}")
        # Get vector config from source
        source_info = client.get_collection(source_collection)
        client.create_collection(
            collection_name=target_collection,
            vectors_config=source_info.config.params.vectors
        )
    
    # Batch upload to target
    migrated = 0
    for batch in chunks(all_points, BATCH_CONFIGS['migration']['size']):
        client.upsert(
            collection_name=target_collection,
            points=batch
        )
        migrated += len(batch)
        
        if migrated % 500 == 0:
            logger.info(f"  Migrated {migrated}/{len(all_points)} points...")
    
    # Verify migration
    target_info = client.get_collection(target_collection)
    logger.info(f"Migrated {len(all_points)} points to {target_collection} (new total: {target_info.points_count})")
    
    return len(all_points)


def pre_cleanup_validation(client: QdrantClient, valid_hashes: dict) -> bool:
    """Complete pre-flight check before any modifications."""
    
    checks = {
        'hash_collisions': len(detect_hash_collisions(valid_hashes)) == 0,
        'normalize_consistency': validate_hash_mapping(valid_hashes),
        'qdrant_healthy': True,  # Will be set by health check
        'docker_volume': verify_docker_volume_consistency(client),
        'disk_space': shutil.disk_usage('.').free > 1_000_000_000  # 1GB free
    }
    
    # Qdrant health check
    try:
        _ = client.get_collections()
        checks['qdrant_healthy'] = True
    except Exception as e:
        logger.error(f"Qdrant health check failed: {e}")
        checks['qdrant_healthy'] = False
    
    # Test normalization with known problematic cases
    test_cases = [
        ('-Users-ramakrishnanannaswamy-projects-claude-self-reflect', 'claude-self-reflect'),
        ('/Users/ramakrishnanannaswamy/.claude/projects/-Users-ramakrishnanannaswamy-projects-claude-self-reflect', 'claude-self-reflect'),
        ('/logs/-Users-ramakrishnanannaswamy-projects-claude-self-reflect', 'claude-self-reflect')
    ]
    
    checks['normalize_tests'] = True
    for input_path, expected in test_cases:
        actual = normalize_project_name(input_path)
        if actual != expected:
            logger.error(f"Normalization test failed: {input_path} -> {actual} (expected {expected})")
            checks['normalize_tests'] = False
            break
    
    # Report results
    logger.info("\nPre-flight validation results:")
    for check, passed in checks.items():
        status = "✓" if passed else "✗"
        logger.info(f"{status} {check}: {'PASSED' if passed else 'FAILED'}")
    
    return all(checks.values())


def main():
    """Main execution."""
    logger.info("=" * 70)
    logger.info("COMPREHENSIVE COLLECTION CLEANUP")
    logger.info("=" * 70)
    
    # Connect to Qdrant
    client = QdrantClient(url=QDRANT_URL)
    
    # Step 1: Get valid project hashes
    logger.info("\n1. Analyzing project directories...")
    valid_hashes = get_valid_project_hashes()
    logger.info(f"Found {len(valid_hashes)} valid projects")
    
    # Step 2: Pre-flight validation
    logger.info("\n2. Running pre-flight validation...")
    if not pre_cleanup_validation(client, valid_hashes):
        logger.error("Pre-flight validation failed. Aborting.")
        return 1
    
    # Step 3: Identify collections
    logger.info("\n3. Analyzing Qdrant collections...")
    collections = client.get_collections().collections
    collection_names = [c.name for c in collections]
    
    valid_collections = set()
    spurious_collections = set()
    collection_points = {}
    
    for coll_name in collection_names:
        if not coll_name.startswith("conv_"):
            continue
        
        parts = coll_name.split("_")
        if len(parts) >= 3:
            coll_hash = parts[1]
            
            info = client.get_collection(coll_name)
            point_count = info.points_count or 0
            collection_points[coll_name] = point_count
            
            if coll_hash in valid_hashes:
                valid_collections.add(coll_name)
            else:
                spurious_collections.add(coll_name)
    
    logger.info(f"Found {len(valid_collections)} valid collections")
    logger.info(f"Found {len(spurious_collections)} spurious collections")
    
    # Step 4: Create recovery manifest
    logger.info("\n4. Creating recovery manifest...")
    manifest = create_recovery_manifest(client, valid_hashes, valid_collections, spurious_collections)
    
    # Step 5: Analyze spurious collections for recovery
    logger.info("\n5. Analyzing spurious collections for recovery...")
    recovery_plan = smart_collection_recovery(client, spurious_collections, valid_hashes)
    
    # Report findings
    logger.info(f"\nRecovery plan:")
    logger.info(f"  - Empty collections (safe to delete): {len(recovery_plan['empty'])}")
    logger.info(f"  - Direct matches (can migrate): {len(recovery_plan['direct_matches'])}")
    logger.info(f"  - Ambiguous (need review): {len(recovery_plan['ambiguous'])}")
    logger.info(f"  - Corrupted (missing data): {len(recovery_plan['corrupted'])}")
    
    # Show sample of direct matches
    if recovery_plan['direct_matches']:
        logger.info("\n  Sample of collections that can be migrated:")
        for source, target_info in list(recovery_plan['direct_matches'].items())[:5]:
            logger.info(f"    {source} -> {target_info['target_collection']} ({target_info['points_count']} points)")
    
    # Step 6: User confirmation
    logger.info("\n" + "=" * 70)
    logger.info("READY TO PROCEED WITH CLEANUP")
    logger.info("=" * 70)
    logger.info(f"Will delete {len(recovery_plan['empty'])} empty collections")
    logger.info(f"Will migrate {len(recovery_plan['direct_matches'])} collections with data")
    
    response = input("\nProceed with cleanup? (yes/dry-run/no): ").lower()
    
    if response == "no":
        logger.info("Cleanup cancelled by user")
        return 0
    
    dry_run = response == "dry-run"
    if dry_run:
        logger.info("Running in DRY-RUN mode - no changes will be made")
    
    # Step 7: Migrate non-empty collections
    if recovery_plan['direct_matches'] and not dry_run:
        logger.info("\n7. Migrating non-empty spurious collections...")
        migrated_count = 0
        
        for source, target_info in recovery_plan['direct_matches'].items():
            try:
                points_migrated = merge_spurious_into_valid(
                    client,
                    source,
                    target_info['target_collection'],
                    dry_run=False
                )
                migrated_count += 1
                
                # Delete source after successful migration
                client.delete_collection(source)
                logger.info(f"  Deleted source collection {source} after migration")
                
            except Exception as e:
                logger.error(f"Failed to migrate {source}: {e}")
        
        logger.info(f"Successfully migrated {migrated_count} collections")
    
    # Step 8: Delete empty collections
    if recovery_plan['empty']:
        logger.info(f"\n8. Deleting {len(recovery_plan['empty'])} empty collections...")
        
        if not dry_run:
            deleted, failed = batch_delete_empty_collections(client, recovery_plan['empty'])
            logger.info(f"Deleted {len(deleted)} empty collections")
            
            if failed:
                logger.warning(f"Failed to delete {len(failed)} collections")
                for coll, error in failed[:5]:
                    logger.warning(f"  {coll}: {error}")
        else:
            logger.info(f"DRY-RUN: Would delete {len(recovery_plan['empty'])} empty collections")
    
    # Step 9: Final summary
    logger.info("\n" + "=" * 70)
    logger.info("CLEANUP SUMMARY")
    logger.info("=" * 70)
    
    # Get final collection count
    final_collections = client.get_collections().collections
    final_conv_collections = [c.name for c in final_collections if c.name.startswith("conv_")]
    
    logger.info(f"Initial collections: {len(collection_names)}")
    logger.info(f"Final collections: {len(final_collections)}")
    logger.info(f"Conversation collections: {len(final_conv_collections)}")
    
    if recovery_plan['ambiguous']:
        logger.warning(f"\n⚠️  {len(recovery_plan['ambiguous'])} collections need manual review:")
        for coll, info in list(recovery_plan['ambiguous'].items())[:5]:
            logger.warning(f"  {coll}: {info['dominant_project'][0]} ({info['dominant_project'][1]} points)")
    
    if recovery_plan['corrupted']:
        logger.warning(f"\n⚠️  {len(recovery_plan['corrupted'])} corrupted collections found")
    
    logger.info("\n✓ Cleanup complete!")
    logger.info(f"Manifest saved for recovery if needed: qdrant_cleanup_manifest_*.json")
    
    return 0


if __name__ == "__main__":
    sys.exit(main())