#!/usr/bin/env python3
"""
Fix misplaced conversations by moving them to correct collections.

This script:
1. Identifies misplaced conversations
2. Creates correct collections if needed
3. Moves points to correct collections
4. Removes them from wrong collections
"""

import asyncio
import hashlib
import logging
from pathlib import Path
from typing import Dict, List, Any
import json
import sys

# Add parent directory to path to import utils
sys.path.insert(0, str(Path(__file__).parent))
from utils import normalize_project_name

from qdrant_client import AsyncQdrantClient
from qdrant_client.models import PointStruct, VectorParams, Distance

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

class CollectionFixer:
    def __init__(self, qdrant_url: str = "http://localhost:6333"):
        self.client = AsyncQdrantClient(url=qdrant_url)
        self.vector_size = 384  # FastEmbed all-MiniLM-L6-v2
        self.misplaced = {}
        self.dry_run = True  # Safety first
    
    def get_expected_collection(self, project_path: str) -> str:
        """Calculate expected collection name for a project."""
        # Normalize the project name first (like MCP server does)
        normalized = normalize_project_name(project_path)
        hash_val = hashlib.md5(normalized.encode()).hexdigest()[:8]
        return f"conv_{hash_val}_local"
    
    async def ensure_collection_exists(self, collection_name: str):
        """Ensure a collection exists."""
        try:
            await self.client.get_collection(collection_name)
            logger.info(f"Collection {collection_name} already exists")
        except:
            logger.info(f"Creating collection {collection_name}")
            if not self.dry_run:
                await self.client.create_collection(
                    collection_name=collection_name,
                    vectors_config=VectorParams(
                        size=self.vector_size,
                        distance=Distance.COSINE
                    )
                )
    
    async def analyze_collections(self) -> Dict[str, List[Dict]]:
        """Find all misplaced conversations."""
        collections = await self.client.get_collections()
        misplaced_by_collection = {}
        
        for collection in collections.collections:
            coll_name = collection.name
            if not coll_name.startswith('conv_') or not coll_name.endswith('_local'):
                continue
            
            # Get collection hash
            coll_hash = coll_name.replace('conv_', '').replace('_local', '')
            
            # Scroll through all points
            offset = None
            misplaced_in_this = []
            
            while True:
                result = await self.client.scroll(
                    collection_name=coll_name,
                    limit=100,
                    offset=offset,
                    with_payload=True,
                    with_vectors=True
                )
                
                points = result[0]
                if not points:
                    break
                
                for point in points:
                    project = point.payload.get('project', 'unknown')
                    expected_coll = self.get_expected_collection(project)
                    
                    if expected_coll != coll_name:
                        misplaced_in_this.append({
                            'point_id': point.id,
                            'conversation_id': point.payload.get('conversation_id'),
                            'project': project,
                            'current_collection': coll_name,
                            'expected_collection': expected_coll,
                            'vector': point.vector,
                            'payload': point.payload
                        })
                
                if len(points) < 100:
                    break
                offset = points[-1].id
            
            if misplaced_in_this:
                misplaced_by_collection[coll_name] = misplaced_in_this
                logger.info(f"Found {len(misplaced_in_this)} misplaced points in {coll_name}")
        
        return misplaced_by_collection
    
    async def fix_misplaced_points(self, misplaced: Dict[str, List[Dict]]):
        """Move misplaced points to correct collections."""
        total_fixed = 0
        
        for current_coll, points in misplaced.items():
            logger.info(f"\nProcessing {len(points)} misplaced points from {current_coll}")
            
            # Group by target collection
            by_target = {}
            for point in points:
                target = point['expected_collection']
                if target not in by_target:
                    by_target[target] = []
                by_target[target].append(point)
            
            # Process each target collection
            for target_coll, target_points in by_target.items():
                logger.info(f"  Moving {len(target_points)} points to {target_coll}")
                
                # Ensure target collection exists
                await self.ensure_collection_exists(target_coll)
                
                if not self.dry_run:
                    # Prepare points for insertion with new IDs to avoid conflicts
                    new_points = []
                    for p in target_points:
                        # Generate new ID based on payload to avoid conflicts
                        import uuid
                        new_id = str(uuid.uuid4())
                        new_points.append(PointStruct(
                            id=new_id,
                            vector=p['vector'],
                            payload=p['payload']
                        ))
                    
                    # Insert into correct collection
                    try:
                        await self.client.upsert(
                            collection_name=target_coll,
                            points=new_points,
                            wait=True
                        )
                        logger.info(f"    Successfully moved {len(new_points)} points")
                        
                        # Delete from wrong collection only after successful insert
                        point_ids = [p['point_id'] for p in target_points]
                        await self.client.delete(
                            collection_name=current_coll,
                            points_selector=point_ids
                        )
                        logger.info(f"    Deleted {len(point_ids)} points from {current_coll}")
                    except Exception as e:
                        logger.error(f"    Failed to move points: {e}")
                        continue
                    
                    total_fixed += len(target_points)
                    logger.info(f"    Moved {len(target_points)} points")
                else:
                    logger.info(f"    [DRY RUN] Would move {len(target_points)} points")
        
        return total_fixed
    
    async def run(self, dry_run: bool = True):
        """Main execution."""
        self.dry_run = dry_run
        
        logger.info(f"Starting collection fix (dry_run={dry_run})")
        
        # Analyze
        logger.info("\n=== ANALYZING COLLECTIONS ===")
        misplaced = await self.analyze_collections()
        
        if not misplaced:
            logger.info("No misplaced conversations found!")
            return
        
        # Summary
        total_misplaced = sum(len(points) for points in misplaced.values())
        logger.info(f"\n=== SUMMARY ===")
        logger.info(f"Total misplaced points: {total_misplaced}")
        logger.info(f"Affected collections: {len(misplaced)}")
        
        # Show details
        for coll, points in misplaced.items():
            logger.info(f"\n{coll}:")
            # Group by project
            by_project = {}
            for p in points:
                proj = p['project']
                if proj not in by_project:
                    by_project[proj] = []
                by_project[proj].append(p)
            
            for proj, proj_points in by_project.items():
                target = proj_points[0]['expected_collection']
                logger.info(f"  {proj}: {len(proj_points)} points → {target}")
        
        if not dry_run:
            logger.info("\n=== FIXING MISPLACED POINTS ===")
            fixed = await self.fix_misplaced_points(misplaced)
            logger.info(f"\nFixed {fixed} points")
        else:
            logger.info("\n[DRY RUN] No changes made. Run with --fix to apply changes")
    
    async def close(self):
        """Clean up."""
        await self.client.close()

async def main():
    import sys
    
    dry_run = "--fix" not in sys.argv
    
    fixer = CollectionFixer()
    try:
        await fixer.run(dry_run=dry_run)
    finally:
        await fixer.close()

if __name__ == "__main__":
    asyncio.run(main())