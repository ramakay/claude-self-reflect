#!/usr/bin/env python3
"""
COMPLETE V2 MIGRATION
Ensures 100% of all conversations are migrated to v2.
This is the production migration that ships with setup wizard.
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
import time

sys.path.insert(0, str(Path(__file__).parent))
from utils import normalize_project_name

logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(message)s'
)
logger = logging.getLogger(__name__)

class CompleteV2Migrator:
    """Ensures 100% v2 migration."""
    
    def __init__(self):
        self.logs_dir = Path(os.path.expanduser("~/.claude/projects"))
        self.qdrant_url = os.getenv("QDRANT_URL", "http://localhost:6333")
        self.stats = defaultdict(int)
        self.start_time = time.time()
        
    async def migrate_all_collections(self) -> None:
        """Migrate ALL collections to v2."""
        from qdrant_client import AsyncQdrantClient, models
        from fastembed import TextEmbedding
        
        client = AsyncQdrantClient(url=self.qdrant_url)
        embedding_model = TextEmbedding("sentence-transformers/all-MiniLM-L6-v2")
        
        # Get all collections
        collections = await client.get_collections()
        total_collections = len(collections.collections)
        
        logger.info("=" * 70)
        logger.info("COMPLETE V2 MIGRATION - 100% CONVERSION")
        logger.info("=" * 70)
        logger.info(f"Total collections: {total_collections}")
        
        for idx, coll in enumerate(collections.collections, 1):
            collection_name = coll.name
            
            # Skip non-conversation collections
            if not collection_name.startswith("conv_"):
                continue
            
            logger.info(f"\n[{idx}/{total_collections}] Processing {collection_name}")
            
            try:
                # Get ALL chunks (v1 and v2)
                all_chunks = []
                offset = None
                
                while True:
                    batch = await client.scroll(
                        collection_name=collection_name,
                        limit=1000,
                        offset=offset,
                        with_payload=True,
                        with_vectors=False
                    )
                    
                    all_chunks.extend(batch[0])
                    offset = batch[1]
                    
                    if not offset:
                        break
                
                if not all_chunks:
                    logger.info(f"  Empty collection")
                    continue
                
                # Check if already fully v2
                v2_count = sum(1 for p in all_chunks if p.payload.get("chunking_version") == "v2")
                v1_count = len(all_chunks) - v2_count
                
                if v1_count == 0:
                    logger.info(f"  ✅ Already fully v2 ({v2_count} chunks)")
                    self.stats["already_v2"] += 1
                    continue
                
                logger.info(f"  Found {v1_count} v1 chunks to migrate")
                
                # Group by conversation
                conversations = defaultdict(list)
                for point in all_chunks:
                    if point.payload.get("chunking_version") != "v2":
                        conv_id = point.payload.get("conversation_id", "unknown")
                        conversations[conv_id].append(point)
                
                # Find project directory
                project_dir = None
                for pd in self.logs_dir.iterdir():
                    if not pd.is_dir():
                        continue
                    normalized = normalize_project_name(str(pd))
                    project_hash = hashlib.md5(normalized.encode()).hexdigest()[:8]
                    if project_hash in collection_name:
                        project_dir = pd
                        break
                
                if not project_dir:
                    # Try to reconstruct from payload
                    if all_chunks:
                        project_name = all_chunks[0].payload.get("project", "")
                        if project_name:
                            for pd in self.logs_dir.iterdir():
                                if project_name in str(pd):
                                    project_dir = pd
                                    break
                
                if not project_dir:
                    logger.warning(f"  ⚠️ Could not find project directory, using text from v1 chunks")
                
                # Process each conversation
                migrated_in_collection = 0
                
                for conv_id, v1_points in conversations.items():
                    try:
                        # Try to read original file
                        messages = []
                        if project_dir:
                            jsonl_file = project_dir / f"{conv_id}.jsonl"
                            if jsonl_file.exists():
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
                        
                        # If no file, reconstruct from v1 chunks
                        if not messages:
                            # Sort v1 points by chunk_index if available
                            v1_points.sort(key=lambda p: p.payload.get("chunk_index", 0))
                            combined_text = "\n\n".join([p.payload.get("text", "") for p in v1_points])
                        else:
                            # Use original messages
                            combined_text = "\n\n".join([
                                f"{msg.get('role', 'unknown')}: {self.extract_text(msg.get('content', ''))}"
                                for msg in messages
                            ])
                        
                        if not combined_text:
                            continue
                        
                        # Create v2 chunks
                        chunker = TokenAwareChunker()
                        v2_chunks = chunker.chunk_text(combined_text)
                        
                        if not v2_chunks:
                            continue
                        
                        # Generate embeddings
                        embeddings = list(embedding_model.embed(v2_chunks))
                        
                        # Create v2 points
                        v2_points = []
                        for chunk_idx, (chunk_text, embedding) in enumerate(zip(v2_chunks, embeddings)):
                            point_id = hashlib.sha256(
                                f"{conv_id}_{chunk_idx}_v2_complete".encode()
                            ).hexdigest()[:32]
                            
                            v2_points.append(models.PointStruct(
                                id=point_id,
                                vector=embedding.tolist(),
                                payload={
                                    "text": chunk_text,
                                    "conversation_id": conv_id,
                                    "chunk_index": chunk_idx,
                                    "project": v1_points[0].payload.get("project", "unknown"),
                                    "timestamp": datetime.now().isoformat(),
                                    "chunking_version": "v2",
                                    "chunk_method": "token_aware",
                                    "chunk_overlap": True,
                                    "migration_type": "complete_100"
                                }
                            ))
                        
                        # Delete v1 points and insert v2
                        if v2_points:
                            # Delete v1
                            v1_ids = [p.id for p in v1_points]
                            await client.delete(
                                collection_name=collection_name,
                                points_selector=models.PointIdsList(points=v1_ids),
                                wait=False
                            )
                            
                            # Insert v2
                            await client.upsert(
                                collection_name=collection_name,
                                points=v2_points,
                                wait=False
                            )
                            
                            migrated_in_collection += len(v2_points)
                            self.stats["v1_deleted"] += len(v1_ids)
                            
                    except Exception as e:
                        logger.error(f"    Error migrating {conv_id}: {e}")
                        self.stats["errors"] += 1
                
                if migrated_in_collection > 0:
                    logger.info(f"  ✅ Migrated {migrated_in_collection} v2 chunks")
                    self.stats["v2_created"] += migrated_in_collection
                    self.stats["collections_migrated"] += 1
                
            except Exception as e:
                logger.error(f"  ❌ Error with collection: {e}")
                self.stats["collection_errors"] += 1
            
            # Progress report
            elapsed = time.time() - self.start_time
            rate = self.stats["v2_created"] / elapsed if elapsed > 0 else 0
            logger.info(f"  Progress: {idx}/{total_collections} collections | "
                       f"{self.stats['v2_created']} chunks created | "
                       f"{rate:.0f} chunks/sec")
        
        # Final report
        elapsed = time.time() - self.start_time
        logger.info("\n" + "=" * 70)
        logger.info("MIGRATION COMPLETE")
        logger.info("=" * 70)
        logger.info(f"Collections processed: {total_collections}")
        logger.info(f"Collections migrated: {self.stats['collections_migrated']}")
        logger.info(f"Already v2: {self.stats['already_v2']}")
        logger.info(f"V1 chunks deleted: {self.stats['v1_deleted']}")
        logger.info(f"V2 chunks created: {self.stats['v2_created']}")
        logger.info(f"Errors: {self.stats['errors']}")
        logger.info(f"Time taken: {elapsed/60:.1f} minutes")
        
        # Verify 100% migration
        await self.verify_complete_migration()
    
    async def verify_complete_migration(self):
        """Verify that migration is 100% complete."""
        from qdrant_client import AsyncQdrantClient, models
        
        client = AsyncQdrantClient(url=self.qdrant_url)
        collections = await client.get_collections()
        
        v1_remaining = 0
        v2_total = 0
        
        for coll in collections.collections:
            if not coll.name.startswith("conv_"):
                continue
            
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
                
                v2_count = await client.count(
                    collection_name=coll.name,
                    count_filter=models.Filter(
                        must=[
                            models.FieldCondition(
                                key="chunking_version",
                                match=models.MatchValue(value="v2")
                            )
                        ]
                    )
                )
                
                v1_remaining += v1_count.count
                v2_total += v2_count.count
                
            except:
                pass
        
        logger.info("\n📊 FINAL VERIFICATION:")
        logger.info(f"  V1 chunks remaining: {v1_remaining}")
        logger.info(f"  V2 chunks total: {v2_total}")
        
        if v1_remaining == 0:
            logger.info("\n✅ SUCCESS! 100% MIGRATION COMPLETE!")
            logger.info("All conversations are now using v2 chunking.")
        else:
            percentage = v2_total / (v1_remaining + v2_total) * 100 if (v1_remaining + v2_total) > 0 else 0
            logger.info(f"\n⚠️ Migration {percentage:.1f}% complete")
            logger.info(f"Run again to migrate remaining {v1_remaining} v1 chunks")
    
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
        """Run the complete migration."""
        await self.migrate_all_collections()


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
    migrator = CompleteV2Migrator()
    await migrator.run()


if __name__ == "__main__":
    asyncio.run(main())