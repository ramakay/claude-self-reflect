#!/usr/bin/env python3
"""
FAST PARALLEL V2 MIGRATION
Migrates all v1 chunks to v2 using parallel processing.
Designed to complete in minutes, not hours.
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
from concurrent.futures import ThreadPoolExecutor
import time

sys.path.insert(0, str(Path(__file__).parent))
from utils import normalize_project_name

logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

class FastParallelMigrator:
    """Ultra-fast parallel migration to v2."""
    
    def __init__(self, workers: int = 10):
        self.logs_dir = Path(os.path.expanduser("~/.claude/projects"))
        self.qdrant_url = os.getenv("QDRANT_URL", "http://localhost:6333")
        self.workers = workers
        self.stats = {
            "collections_processed": 0,
            "chunks_migrated": 0,
            "errors": 0,
            "start_time": time.time()
        }
        
    async def get_collections_needing_migration(self) -> List[str]:
        """Get all collections with v1 chunks."""
        from qdrant_client import AsyncQdrantClient, models
        
        client = AsyncQdrantClient(url=self.qdrant_url)
        collections = await client.get_collections()
        
        needs_migration = []
        
        for coll in collections.collections:
            try:
                v1_count = await client.count(
                    collection_name=coll.name,
                    count_filter=models.Filter(
                        must_not=[
                            models.FieldCondition(
                                key="chunking_version",
                                match=models.MatchValue(value="v2")
                            )
                        ]
                    )
                )
                
                if v1_count.count > 0:
                    needs_migration.append({
                        "name": coll.name,
                        "v1_count": v1_count.count
                    })
            except:
                pass
        
        return needs_migration
    
    async def migrate_collection_fast(self, collection_info: Dict) -> int:
        """Migrate a single collection quickly."""
        from fastembed import TextEmbedding
        from qdrant_client import AsyncQdrantClient, models
        
        collection_name = collection_info["name"]
        logger.info(f"Processing {collection_name} ({collection_info['v1_count']} v1 chunks)")
        
        try:
            client = AsyncQdrantClient(url=self.qdrant_url)
            embedding_model = TextEmbedding("sentence-transformers/all-MiniLM-L6-v2")
            
            # Get ALL v1 chunks at once
            v1_chunks = []
            offset = None
            
            while True:
                batch = await client.scroll(
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
                    offset=offset,
                    with_payload=True,
                    with_vectors=False
                )
                
                v1_chunks.extend(batch[0])
                offset = batch[1]
                
                if not offset:
                    break
            
            if not v1_chunks:
                return 0
            
            # Group by conversation
            conversations = {}
            for point in v1_chunks:
                conv_id = point.payload.get("conversation_id", "unknown")
                if conv_id not in conversations:
                    conversations[conv_id] = []
                conversations[conv_id].append(point)
            
            # Find project path
            project_path = None
            for project_dir in self.logs_dir.iterdir():
                if not project_dir.is_dir():
                    continue
                normalized = normalize_project_name(str(project_dir))
                project_hash = hashlib.md5(normalized.encode()).hexdigest()[:8]
                if f"conv_{project_hash}" in collection_name:
                    project_path = project_dir
                    break
            
            if not project_path:
                logger.warning(f"Could not find project for {collection_name}")
                return 0
            
            total_migrated = 0
            
            # Process conversations in parallel batches
            for conv_id, old_points in conversations.items():
                jsonl_file = project_path / f"{conv_id}.jsonl"
                
                if not jsonl_file.exists():
                    continue
                
                # Read and process
                try:
                    messages = []
                    with open(jsonl_file) as f:
                        for line in f:
                            try:
                                data = json.loads(line.strip())
                                if 'message' in data:
                                    messages.append(data['message'])
                                elif 'role' in data:
                                    messages.append(data)
                            except:
                                pass
                    
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
                    
                    # Generate embeddings in batch
                    embeddings = list(embedding_model.embed(v2_chunks))
                    
                    # Create v2 points
                    v2_points = []
                    for idx, (chunk_text, embedding) in enumerate(zip(v2_chunks, embeddings)):
                        point_id = hashlib.sha256(
                            f"{conv_id}_{idx}_v2_fast".encode()
                        ).hexdigest()[:32]
                        
                        v2_points.append(models.PointStruct(
                            id=point_id,
                            vector=embedding.tolist(),
                            payload={
                                "text": chunk_text,
                                "conversation_id": conv_id,
                                "chunk_index": idx,
                                "project": old_points[0].payload.get("project", "unknown"),
                                "timestamp": datetime.now().isoformat(),
                                "chunking_version": "v2",
                                "chunk_method": "token_aware",
                                "chunk_overlap": True,
                                "migration_type": "fast_parallel"
                            }
                        ))
                    
                    # Upsert v2 points
                    if v2_points:
                        await client.upsert(
                            collection_name=collection_name,
                            points=v2_points,
                            wait=False  # Don't wait for indexing
                        )
                        
                        # Delete old points
                        old_ids = [p.id for p in old_points]
                        await client.delete(
                            collection_name=collection_name,
                            points_selector=models.PointIdsList(points=old_ids),
                            wait=False
                        )
                        
                        total_migrated += len(v2_points)
                
                except Exception as e:
                    logger.error(f"Error processing {conv_id}: {e}")
                    self.stats["errors"] += 1
            
            logger.info(f"  ✅ {collection_name}: migrated {total_migrated} chunks")
            return total_migrated
            
        except Exception as e:
            logger.error(f"Error with {collection_name}: {e}")
            self.stats["errors"] += 1
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
    
    async def run(self):
        """Run fast parallel migration."""
        logger.info("=" * 70)
        logger.info("FAST PARALLEL V2 MIGRATION")
        logger.info("=" * 70)
        logger.info(f"Workers: {self.workers}")
        
        # Get collections needing migration
        collections = await self.get_collections_needing_migration()
        
        if not collections:
            logger.info("✅ All collections already migrated!")
            return
        
        total_v1 = sum(c["v1_count"] for c in collections)
        logger.info(f"Collections to migrate: {len(collections)}")
        logger.info(f"Total v1 chunks: {total_v1}")
        logger.info(f"Estimated time: {total_v1 / 100 / 60:.1f} minutes")
        logger.info("")
        
        # Process collections in parallel
        semaphore = asyncio.Semaphore(self.workers)
        
        async def process_with_limit(coll):
            async with semaphore:
                chunks = await self.migrate_collection_fast(coll)
                self.stats["collections_processed"] += 1
                self.stats["chunks_migrated"] += chunks
                
                # Progress report
                elapsed = time.time() - self.stats["start_time"]
                rate = self.stats["chunks_migrated"] / elapsed if elapsed > 0 else 0
                remaining = total_v1 - self.stats["chunks_migrated"]
                eta = remaining / rate if rate > 0 else 0
                
                logger.info(f"Progress: {self.stats['chunks_migrated']}/{total_v1} " +
                          f"({self.stats['chunks_migrated']/total_v1*100:.1f}%) " +
                          f"Rate: {rate:.0f} chunks/sec " +
                          f"ETA: {eta/60:.1f} min")
        
        # Run all migrations in parallel
        tasks = [process_with_limit(coll) for coll in collections]
        await asyncio.gather(*tasks)
        
        # Final report
        elapsed = time.time() - self.stats["start_time"]
        logger.info("\n" + "=" * 70)
        logger.info("MIGRATION COMPLETE")
        logger.info("=" * 70)
        logger.info(f"Collections processed: {self.stats['collections_processed']}")
        logger.info(f"Chunks migrated: {self.stats['chunks_migrated']}")
        logger.info(f"Errors: {self.stats['errors']}")
        logger.info(f"Time taken: {elapsed/60:.1f} minutes")
        logger.info(f"Average rate: {self.stats['chunks_migrated']/elapsed:.0f} chunks/sec")
        logger.info("\n✅ All conversations migrated to v2!")


class TokenAwareChunker:
    """Token-aware chunking implementation."""
    
    def __init__(self, chunk_size_tokens: int = 400, chunk_overlap_tokens: int = 75):
        self.chunk_size_chars = chunk_size_tokens * 4
        self.chunk_overlap_chars = chunk_overlap_tokens * 4
    
    def chunk_text(self, text: str) -> List[str]:
        """Chunk text with token awareness and overlap."""
        if not text or len(text) <= self.chunk_size_chars:
            return [text] if text else []
        
        chunks = []
        start = 0
        
        while start < len(text):
            end = min(start + self.chunk_size_chars, len(text))
            
            if end < len(text):
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
    parser = argparse.ArgumentParser(description="Fast parallel v2 migration")
    parser.add_argument("--workers", type=int, default=10, help="Number of parallel workers")
    args = parser.parse_args()
    
    migrator = FastParallelMigrator(workers=args.workers)
    await migrator.run()


if __name__ == "__main__":
    asyncio.run(main())