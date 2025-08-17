#!/usr/bin/env python3
"""
Incremental Repair Script - Fix missing chunks without full re-import.
Only processes chunks that failed to create embeddings.
"""

import asyncio
import json
import hashlib
import os
import tempfile
from pathlib import Path
from typing import Dict, List, Set, Tuple, Optional
from datetime import datetime
import logging

from qdrant_client import AsyncQdrantClient
from qdrant_client import models
from dotenv import load_dotenv

# Add parent directory to path for imports
import sys
sys.path.append(str(Path(__file__).parent))

from utils import normalize_project_name

load_dotenv()

# Configure logging
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

class ChunkRepairer:
    """Repairs missing chunks in Qdrant collections."""
    
    def __init__(self):
        self.client = AsyncQdrantClient(
            url=os.getenv("QDRANT_URL", "http://localhost:6333")
        )
        self.logs_dir = Path.home() / ".claude" / "projects"
        self.chunk_size = 15  # Messages per chunk
        self.stats = {
            "collections_checked": 0,
            "missing_chunks_found": 0,
            "chunks_repaired": 0,
            "errors": 0
        }
    
    def get_project_hash(self, project_name: str) -> str:
        """Get consistent hash for project name."""
        normalized = normalize_project_name(project_name)
        return hashlib.md5(normalized.encode()).hexdigest()[:8]
    
    def get_collection_name(self, project_name: str, use_local: bool = True) -> str:
        """Generate collection name for project."""
        project_hash = self.get_project_hash(project_name)
        suffix = "_local" if use_local else "_voyage"
        return f"conv_{project_hash}{suffix}"
    
    async def get_existing_chunks(self, collection_name: str) -> Set[Tuple[str, int]]:
        """Get all existing (conversation_id, chunk_index) pairs from collection."""
        existing = set()
        
        try:
            # Check if collection exists
            collections = await self.client.get_collections()
            if collection_name not in [c.name for c in collections.collections]:
                logger.warning(f"Collection {collection_name} does not exist")
                return existing
            
            # Scroll through all points to get existing chunks
            offset = None
            while True:
                # Optimized batch size per Qdrant recommendations
                result, next_offset = await self.client.scroll(
                    collection_name=collection_name,
                    limit=1000,  # Increased for better throughput
                    offset=offset,
                    with_payload=["conversation_id", "chunk_index"]  # Only needed fields
                )
                
                for point in result:
                    conv_id = point.payload.get('conversation_id', '')
                    chunk_idx = point.payload.get('chunk_index', -1)
                    if conv_id and chunk_idx >= 0:
                        existing.add((conv_id, chunk_idx))
                
                if next_offset is None:
                    break
                offset = next_offset
                
        except Exception as e:
            logger.error(f"Error getting existing chunks from {collection_name}: {e}")
        
        return existing
    
    def count_expected_chunks(self, jsonl_file: Path) -> int:
        """Count how many chunks should exist for a JSONL file."""
        try:
            message_count = 0
            with open(jsonl_file, 'r') as f:
                for line in f:
                    if line.strip():
                        try:
                            data = json.loads(line)
                            # Only count actual messages, not metadata
                            if not data.get('isMeta', False):
                                message_count += 1
                        except json.JSONDecodeError:
                            continue
            
            # Calculate expected chunks
            expected_chunks = (message_count + self.chunk_size - 1) // self.chunk_size
            return expected_chunks
            
        except Exception as e:
            logger.error(f"Error counting chunks for {jsonl_file}: {e}")
            return 0
    
    async def identify_missing_chunks(self, project_name: str) -> List[Tuple[Path, List[int]]]:
        """Identify all missing chunks for a project."""
        missing = []
        
        # Get collection name
        collection_name = self.get_collection_name(project_name)
        
        # Get existing chunks
        existing_chunks = await self.get_existing_chunks(collection_name)
        logger.info(f"Found {len(existing_chunks)} existing chunks in {collection_name}")
        
        # Find project directory
        project_dir = None
        for potential_dir in self.logs_dir.glob("*"):
            if potential_dir.is_dir():
                normalized = normalize_project_name(potential_dir.name)
                if normalized == normalize_project_name(project_name):
                    project_dir = potential_dir
                    break
        
        if not project_dir:
            logger.error(f"Project directory not found for {project_name}")
            return missing
        
        logger.info(f"Checking project directory: {project_dir}")
        
        # Check each JSONL file
        for jsonl_file in project_dir.glob("*.jsonl"):
            conversation_id = jsonl_file.stem
            expected_chunks = self.count_expected_chunks(jsonl_file)
            
            # Find missing chunks
            file_missing = []
            for chunk_idx in range(expected_chunks):
                if (conversation_id, chunk_idx) not in existing_chunks:
                    file_missing.append(chunk_idx)
                    self.stats["missing_chunks_found"] += 1
            
            if file_missing:
                missing.append((jsonl_file, file_missing))
                logger.info(f"File {jsonl_file.name}: {len(file_missing)} missing chunks out of {expected_chunks}")
        
        return missing
    
    async def repair_project(self, project_name: str, dry_run: bool = False) -> None:
        """Repair missing chunks for a project."""
        self.stats["collections_checked"] += 1
        
        logger.info(f"\n{'='*60}")
        logger.info(f"Repairing project: {project_name}")
        logger.info(f"{'='*60}")
        
        # Identify missing chunks
        missing_chunks = await self.identify_missing_chunks(project_name)
        
        if not missing_chunks:
            logger.info(f"✅ No missing chunks found for {project_name}")
            return
        
        total_files = len(missing_chunks)
        total_missing = sum(len(chunks) for _, chunks in missing_chunks)
        
        logger.info(f"Found {total_missing} missing chunks across {total_files} files")
        
        if dry_run:
            logger.info("DRY RUN - Would repair the following:")
            for file, chunks in missing_chunks[:5]:  # Show first 5
                logger.info(f"  {file.name}: chunks {chunks[:10]}...")
            return
        
        # Mark files for re-processing
        logger.info(f"Marking {total_files} files for re-processing...")
        
        # Remove these files from imported state to trigger re-import
        state_file = Path("/tmp/imported-files.json")
        if state_file.exists():
            with open(state_file, 'r') as f:
                state = json.load(f)
            
            imported_files = state.get("imported_files", {})
            
            for jsonl_file, _ in missing_chunks:
                file_key = str(jsonl_file)
                if file_key in imported_files:
                    logger.info(f"Removing {jsonl_file.name} from imported state")
                    del imported_files[file_key]
                    self.stats["chunks_repaired"] += len(_)
            
            # Atomic write to prevent corruption
            with tempfile.NamedTemporaryFile(mode='w', dir=state_file.parent, 
                                            delete=False, suffix='.tmp') as tmp:
                json.dump(state, tmp, indent=2)
                tmp_path = Path(tmp.name)
            
            # Atomic rename
            tmp_path.replace(state_file)
            
            logger.info(f"✅ Marked {total_files} files for re-import")
        else:
            logger.warning("State file not found - files will be re-imported on next run")
    
    async def verify_repair(self, project_name: str) -> float:
        """Verify repair success rate."""
        collection_name = self.get_collection_name(project_name)
        
        # Count expected total
        expected_total = 0
        project_dir = None
        
        for potential_dir in self.logs_dir.glob("*"):
            if potential_dir.is_dir():
                normalized = normalize_project_name(potential_dir.name)
                if normalized == normalize_project_name(project_name):
                    project_dir = potential_dir
                    break
        
        if not project_dir:
            return 0.0
        
        for jsonl_file in project_dir.glob("*.jsonl"):
            expected_total += self.count_expected_chunks(jsonl_file)
        
        # Get actual count
        try:
            collection_info = await self.client.get_collection(collection_name)
            actual_points = collection_info.points_count
        except:
            actual_points = 0
        
        success_rate = (actual_points / expected_total * 100) if expected_total > 0 else 0
        
        logger.info(f"\n📊 Verification for {project_name}:")
        logger.info(f"  Expected chunks: {expected_total}")
        logger.info(f"  Actual points: {actual_points}")
        logger.info(f"  Success rate: {success_rate:.1f}%")
        
        return success_rate
    
    async def run(self, project_names: Optional[List[str]] = None, dry_run: bool = False):
        """Run repair for specified projects or all projects."""
        
        if not project_names:
            # Find all projects with collections
            collections = await self.client.get_collections()
            project_names = []
            
            for collection in collections.collections:
                if collection.name.startswith("conv_"):
                    # Extract project from collection
                    # This is approximate - would need reverse lookup
                    project_names.append(collection.name)
        
        logger.info(f"Starting repair for {len(project_names)} projects")
        logger.info(f"Dry run: {dry_run}")
        
        for project in project_names:
            try:
                await self.repair_project(project, dry_run)
                
                if not dry_run:
                    # Verify repair
                    success_rate = await self.verify_repair(project)
                    
                    if success_rate < 90:
                        logger.warning(f"⚠️  Low success rate for {project}: {success_rate:.1f}%")
                
            except Exception as e:
                logger.error(f"Error repairing {project}: {e}")
                self.stats["errors"] += 1
        
        # Print summary
        logger.info(f"\n{'='*60}")
        logger.info("REPAIR SUMMARY")
        logger.info(f"{'='*60}")
        logger.info(f"Collections checked: {self.stats['collections_checked']}")
        logger.info(f"Missing chunks found: {self.stats['missing_chunks_found']}")
        logger.info(f"Chunks marked for repair: {self.stats['chunks_repaired']}")
        logger.info(f"Errors: {self.stats['errors']}")


async def main():
    """Main entry point."""
    import argparse
    
    parser = argparse.ArgumentParser(description="Repair missing chunks in Qdrant collections")
    parser.add_argument("--projects", nargs="+", help="Specific projects to repair")
    parser.add_argument("--dry-run", action="store_true", help="Show what would be repaired without doing it")
    parser.add_argument("--verify-only", action="store_true", help="Only verify success rates")
    
    args = parser.parse_args()
    
    repairer = ChunkRepairer()
    
    if args.verify_only:
        # Just verify
        projects = args.projects or ["anukruti"]
        for project in projects:
            await repairer.verify_repair(project)
    else:
        # Run repair
        await repairer.run(
            project_names=args.projects,
            dry_run=args.dry_run
        )


if __name__ == "__main__":
    asyncio.run(main())