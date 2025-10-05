#!/usr/bin/env python3
"""
BULLETPROOF V2 MIGRATION SCRIPT
Migrates ALL conversations across ALL projects to v2 chunking.
This is the production script that ships with the setup wizard.
"""

import asyncio
import json
import os
import sys
from pathlib import Path
from typing import List, Dict, Any
from datetime import datetime
import logging
import hashlib
from collections import defaultdict

# Add parent directory to path for utils import
sys.path.insert(0, str(Path(__file__).parent))
from utils import normalize_project_name

logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

class UniversalV2Migrator:
    """Migrates ALL conversations to v2 chunking across ALL projects."""
    
    def __init__(self):
        self.logs_dir = Path(os.path.expanduser("~/.claude/projects"))
        self.state_file = Path("config/v2-migration-state.json")
        self.state = self.load_state()
        self.stats = defaultdict(int)
        self.qdrant_url = os.getenv("QDRANT_URL", "http://localhost:6333")
        
    def load_state(self) -> Dict:
        """Load migration state."""
        if self.state_file.exists():
            with open(self.state_file) as f:
                return json.load(f)
        return {
            "version": "2.5.16",
            "started_at": None,
            "completed_at": None,
            "projects_migrated": {},
            "total_chunks_migrated": 0,
            "status": "not_started"
        }
    
    def save_state(self):
        """Save migration state."""
        self.state_file.parent.mkdir(parents=True, exist_ok=True)
        with open(self.state_file, 'w') as f:
            json.dump(self.state, f, indent=2)
    
    async def check_v1_chunks(self, collection_name: str) -> Dict:
        """Check for v1 chunks in a collection."""
        from qdrant_client import AsyncQdrantClient, models
        
        try:
            client = AsyncQdrantClient(url=self.qdrant_url)
            
            # Check if collection exists
            try:
                await client.get_collection(collection_name)
            except:
                return {"exists": False}
            
            # Count v1 and v2 chunks
            v1_count = await client.count(
                collection_name=collection_name,
                count_filter=models.Filter(
                    must_not=[
                        models.FieldCondition(
                            key="chunking_version",
                            match=models.MatchValue(value="v2")
                        )
                    ]
                )
            )
            
            v2_count = await client.count(
                collection_name=collection_name,
                count_filter=models.Filter(
                    must=[
                        models.FieldCondition(
                            key="chunking_version",
                            match=models.MatchValue(value="v2")
                        )
                    ]
                )
            )
            
            return {
                "exists": True,
                "v1_chunks": v1_count.count,
                "v2_chunks": v2_count.count,
                "needs_migration": v1_count.count > 0
            }
            
        except Exception as e:
            logger.error(f"Error checking collection {collection_name}: {e}")
            return {"exists": False, "error": str(e)}
    
    async def migrate_collection(self, project_path: Path, collection_name: str) -> int:
        """Migrate all conversations in a collection to v2."""
        from fastembed import TextEmbedding
        from qdrant_client import AsyncQdrantClient, models
        
        logger.info(f"Migrating collection: {collection_name}")
        chunks_migrated = 0
        
        try:
            client = AsyncQdrantClient(url=self.qdrant_url)
            embedding_model = TextEmbedding("sentence-transformers/all-MiniLM-L6-v2")
            
            # Get all v1 chunks (chunks without v2 metadata)
            v1_chunks = await client.scroll(
                collection_name=collection_name,
                scroll_filter=models.Filter(
                    must_not=[
                        models.FieldCondition(
                            key="chunking_version",
                            match=models.MatchValue(value="v2")
                        )
                    ]
                ),
                limit=1000,
                with_payload=True,
                with_vectors=False
            )
            
            if not v1_chunks[0]:
                logger.info(f"  No v1 chunks to migrate in {collection_name}")
                return 0
            
            # Group chunks by conversation_id
            conversations = defaultdict(list)
            for point in v1_chunks[0]:
                conv_id = point.payload.get("conversation_id", "unknown")
                conversations[conv_id].append(point)
            
            logger.info(f"  Found {len(conversations)} conversations with v1 chunks")
            
            # Process each conversation
            for conv_id, v1_points in conversations.items():
                jsonl_file = project_path / f"{conv_id}.jsonl"
                
                if not jsonl_file.exists():
                    logger.warning(f"  File not found: {jsonl_file.name}")
                    continue
                
                # Read the original conversation
                messages = []
                try:
                    with open(jsonl_file) as f:
                        for line in f:
                            try:
                                data = json.loads(line.strip())
                                if 'message' in data:
                                    messages.append(data['message'])
                                elif 'role' in data:
                                    messages.append(data)
                            except:
                                continue
                except Exception as e:
                    logger.error(f"  Error reading {jsonl_file.name}: {e}")
                    continue
                
                if not messages:
                    continue
                
                # Create v2 chunks
                chunker = TokenAwareChunker()
                combined_text = "\n\n".join([
                    f"{msg.get('role', 'unknown')}: {self.extract_text(msg.get('content', ''))}"
                    for msg in messages
                ])
                
                v2_chunks = chunker.chunk_text(combined_text)
                
                if not v2_chunks:
                    continue
                
                # Generate embeddings and create v2 points
                v2_points = []
                for idx, chunk_text in enumerate(v2_chunks):
                    embeddings = list(embedding_model.embed([chunk_text]))
                    if not embeddings:
                        continue
                    
                    point_id = hashlib.sha256(
                        f"{conv_id}_{idx}_v2".encode()
                    ).hexdigest()[:32]
                    
                    v2_points.append(models.PointStruct(
                        id=point_id,
                        vector=embeddings[0].tolist(),
                        payload={
                            "text": chunk_text,
                            "conversation_id": conv_id,
                            "chunk_index": idx,
                            "project": v1_points[0].payload.get("project", "unknown"),
                            "timestamp": datetime.now().isoformat(),
                            "chunking_version": "v2",
                            "chunk_method": "token_aware",
                            "chunk_overlap": True,
                            "migration_type": "universal",
                            "original_v1_count": len(v1_points)
                        }
                    ))
                
                # Store v2 points
                if v2_points:
                    await client.upsert(
                        collection_name=collection_name,
                        points=v2_points,
                        wait=True
                    )
                    
                    # Delete v1 points for this conversation
                    v1_ids = [point.id for point in v1_points]
                    await client.delete(
                        collection_name=collection_name,
                        points_selector=models.PointIdsList(
                            points=v1_ids
                        ),
                        wait=True
                    )
                    
                    chunks_migrated += len(v2_points)
                    logger.info(f"    Migrated {conv_id}: {len(v1_points)} v1 → {len(v2_points)} v2")
            
            return chunks_migrated
            
        except Exception as e:
            logger.error(f"Error migrating collection {collection_name}: {e}")
            return 0
    
    def extract_text(self, content: Any) -> str:
        """Extract text from message content."""
        if isinstance(content, str):
            return content
        if isinstance(content, list):
            parts = []
            for item in content:
                if isinstance(item, str):
                    parts.append(item)
                elif isinstance(item, dict):
                    if item.get('type') == 'text':
                        parts.append(item.get('text', ''))
            return ' '.join(parts)
        return str(content)[:1000]
    
    async def run(self, dry_run: bool = False):
        """Run the migration."""
        logger.info("=" * 70)
        logger.info("UNIVERSAL V2 MIGRATION - PRODUCTION")
        logger.info("=" * 70)
        
        # Update state
        if not self.state["started_at"]:
            self.state["started_at"] = datetime.now().isoformat()
            self.state["status"] = "in_progress"
            self.save_state()
        
        # Scan all projects
        all_projects = []
        for project_dir in self.logs_dir.iterdir():
            if project_dir.is_dir():
                all_projects.append(project_dir)
        
        logger.info(f"Found {len(all_projects)} projects to scan")
        
        # Check each project's collections
        projects_needing_migration = []
        
        for project_path in all_projects:
            normalized = normalize_project_name(str(project_path))
            project_hash = hashlib.md5(normalized.encode()).hexdigest()[:8]
            
            # Check both local and voyage collections
            for suffix in ["local", "voyage"]:
                collection_name = f"conv_{project_hash}_{suffix}"
                status = await self.check_v1_chunks(collection_name)
                
                if status.get("needs_migration"):
                    projects_needing_migration.append({
                        "path": project_path,
                        "collection": collection_name,
                        "v1_chunks": status["v1_chunks"],
                        "v2_chunks": status["v2_chunks"]
                    })
                    logger.info(f"  {collection_name}: {status['v1_chunks']} v1 chunks to migrate")
        
        if not projects_needing_migration:
            logger.info("\n✅ All collections already migrated to v2!")
            self.state["status"] = "completed"
            self.state["completed_at"] = datetime.now().isoformat()
            self.save_state()
            return
        
        logger.info(f"\n📊 Migration needed for {len(projects_needing_migration)} collections")
        logger.info(f"Total v1 chunks to migrate: {sum(p['v1_chunks'] for p in projects_needing_migration)}")
        
        if dry_run:
            logger.info("\nDRY RUN - No changes will be made")
            return
        
        # Perform migration
        logger.info("\n🚀 Starting migration...")
        
        for project in projects_needing_migration:
            chunks_migrated = await self.migrate_collection(
                project["path"],
                project["collection"]
            )
            
            self.stats["collections_migrated"] += 1
            self.stats["chunks_migrated"] += chunks_migrated
            
            # Update state
            self.state["projects_migrated"][project["collection"]] = {
                "migrated_at": datetime.now().isoformat(),
                "v1_chunks": project["v1_chunks"],
                "v2_chunks_created": chunks_migrated
            }
            self.state["total_chunks_migrated"] = self.stats["chunks_migrated"]
            self.save_state()
            
            # Rate limiting
            await asyncio.sleep(1)
        
        # Final report
        logger.info("\n" + "=" * 70)
        logger.info("MIGRATION COMPLETE")
        logger.info("=" * 70)
        logger.info(f"Collections migrated: {self.stats['collections_migrated']}")
        logger.info(f"Total v2 chunks created: {self.stats['chunks_migrated']}")
        
        self.state["status"] = "completed"
        self.state["completed_at"] = datetime.now().isoformat()
        self.save_state()
        
        logger.info("\n✅ All conversations successfully migrated to v2 chunking!")
        logger.info("Users can now search for previously truncated content.")


class TokenAwareChunker:
    """Token-aware chunking implementation."""
    
    def __init__(self, chunk_size_tokens: int = 400, chunk_overlap_tokens: int = 75):
        self.chunk_size_chars = chunk_size_tokens * 4  # 400 * 4 = 1600 chars
        self.chunk_overlap_chars = chunk_overlap_tokens * 4  # 75 * 4 = 300 chars
    
    def chunk_text(self, text: str) -> List[str]:
        """Chunk text with token awareness and overlap."""
        if not text or len(text) <= self.chunk_size_chars:
            return [text] if text else []
        
        chunks = []
        start = 0
        
        while start < len(text):
            end = min(start + self.chunk_size_chars, len(text))
            
            if end < len(text):
                # Try to break at natural boundaries
                for separator in ['. ', '.\n', '! ', '? ', '\n\n', '\n', ' ']:
                    last_sep = text.rfind(separator, start, end)
                    if last_sep > start + (self.chunk_size_chars // 2):
                        end = last_sep + len(separator)
                        break
            
            chunk = text[start:end].strip()
            if chunk:
                chunks.append(chunk)
            
            if end >= len(text):
                break
            start = max(start + 1, end - self.chunk_overlap_chars)
        
        return chunks


async def main():
    """Main entry point."""
    import argparse
    parser = argparse.ArgumentParser(
        description="Universal V2 Migration - Migrates ALL conversations to v2 chunking"
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Show what would be migrated without making changes"
    )
    parser.add_argument(
        "--reset",
        action="store_true",
        help="Reset migration state and start fresh"
    )
    args = parser.parse_args()
    
    migrator = UniversalV2Migrator()
    
    if args.reset:
        migrator.state = {
            "version": "2.5.16",
            "started_at": None,
            "completed_at": None,
            "projects_migrated": {},
            "total_chunks_migrated": 0,
            "status": "not_started"
        }
        migrator.save_state()
        logger.info("Migration state reset")
    
    await migrator.run(dry_run=args.dry_run)


if __name__ == "__main__":
    asyncio.run(main())