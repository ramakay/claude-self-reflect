#!/usr/bin/env python3
"""
Priority V2 Migration - Fast migration for critical content.
Focuses on TensorZero and other important conversations first.
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

sys.path.insert(0, str(Path(__file__).parent))
from utils import normalize_project_name

logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

class PriorityV2Migrator:
    """Fast migration for priority content."""
    
    def __init__(self):
        self.logs_dir = Path(os.path.expanduser("~/.claude/projects"))
        self.qdrant_url = os.getenv("QDRANT_URL", "http://localhost:6333")
        self.critical_terms = [
            "TensorZero", "Sanskrit", "Observable", "A/B testing",
            "multi-armed bandits", "contextual bandits", "OpenTelemetry"
        ]
        
    async def find_priority_conversations(self) -> List[Dict]:
        """Find conversations with critical content."""
        priority_conversations = []
        
        # Priority projects
        priority_projects = [
            "claude-self-reflect",
            "anukruti",
            "ShopifyMCPMockShop"
        ]
        
        for project_dir in self.logs_dir.iterdir():
            if not project_dir.is_dir():
                continue
            
            # Check if it's a priority project
            project_name = project_dir.name.lower()
            is_priority = any(p in project_name for p in priority_projects)
            
            if not is_priority:
                continue
            
            # Search for critical content
            for jsonl_file in project_dir.glob("*.jsonl"):
                try:
                    # Quick scan for critical terms
                    content = jsonl_file.read_text(errors='ignore')[:100000]  # First 100KB
                    
                    has_critical = any(term in content for term in self.critical_terms)
                    
                    if has_critical:
                        # Get collection name
                        normalized = normalize_project_name(str(project_dir))
                        project_hash = hashlib.md5(normalized.encode()).hexdigest()[:8]
                        
                        priority_conversations.append({
                            "file": jsonl_file,
                            "project": normalized,
                            "collection_local": f"conv_{project_hash}_local",
                            "collection_voyage": f"conv_{project_hash}_voyage",
                            "conversation_id": jsonl_file.stem
                        })
                        
                        logger.info(f"  Priority: {jsonl_file.name} (has critical content)")
                        
                except Exception as e:
                    logger.debug(f"Error scanning {jsonl_file}: {e}")
        
        return priority_conversations
    
    async def migrate_conversation(self, conv_info: Dict) -> int:
        """Migrate a single conversation to v2."""
        from fastembed import TextEmbedding
        from qdrant_client import AsyncQdrantClient, models
        
        logger.info(f"Migrating priority conversation: {conv_info['conversation_id']}")
        
        try:
            client = AsyncQdrantClient(url=self.qdrant_url)
            embedding_model = TextEmbedding("sentence-transformers/all-MiniLM-L6-v2")
            
            # Try local collection first
            collection_name = conv_info['collection_local']
            
            # Ensure collection exists
            try:
                await client.get_collection(collection_name)
            except:
                # Try voyage collection
                collection_name = conv_info['collection_voyage']
                try:
                    await client.get_collection(collection_name)
                except:
                    # Create local collection
                    collection_name = conv_info['collection_local']
                    await client.create_collection(
                        collection_name=collection_name,
                        vectors_config=models.VectorParams(
                            size=384,
                            distance=models.Distance.COSINE
                        )
                    )
            
            # Read conversation
            messages = []
            with open(conv_info['file']) as f:
                for line in f:
                    try:
                        data = json.loads(line.strip())
                        if 'message' in data:
                            messages.append(data['message'])
                        elif 'role' in data:
                            messages.append(data)
                    except:
                        continue
            
            if not messages:
                return 0
            
            # Create v2 chunks
            chunker = TokenAwareChunker()
            combined_text = "\n\n".join([
                f"{msg.get('role', 'unknown')}: {self.extract_text(msg.get('content', ''))}"
                for msg in messages
            ])
            
            v2_chunks = chunker.chunk_text(combined_text)
            
            if not v2_chunks:
                return 0
            
            # Delete existing chunks for this conversation
            await client.delete(
                collection_name=collection_name,
                points_selector=models.Filter(
                    must=[
                        models.FieldCondition(
                            key="conversation_id",
                            match=models.MatchValue(value=conv_info['conversation_id'])
                        )
                    ]
                )
            )
            
            # Create v2 points
            points = []
            for idx, chunk_text in enumerate(v2_chunks):
                embeddings = list(embedding_model.embed([chunk_text]))
                if not embeddings:
                    continue
                
                point_id = hashlib.sha256(
                    f"{conv_info['conversation_id']}_{idx}_v2_priority".encode()
                ).hexdigest()[:32]
                
                points.append(models.PointStruct(
                    id=point_id,
                    vector=embeddings[0].tolist(),
                    payload={
                        "text": chunk_text,
                        "conversation_id": conv_info['conversation_id'],
                        "chunk_index": idx,
                        "project": conv_info['project'],
                        "timestamp": datetime.now().isoformat(),
                        "chunking_version": "v2",
                        "chunk_method": "token_aware",
                        "chunk_overlap": True,
                        "migration_type": "priority"
                    }
                ))
            
            # Store v2 points
            if points:
                await client.upsert(
                    collection_name=collection_name,
                    points=points,
                    wait=True
                )
                
                logger.info(f"  ✅ Created {len(points)} v2 chunks in {collection_name}")
                return len(points)
            
        except Exception as e:
            logger.error(f"  ❌ Error: {e}")
        
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
        """Run priority migration."""
        logger.info("=" * 70)
        logger.info("PRIORITY V2 MIGRATION - CRITICAL CONTENT FIRST")
        logger.info("=" * 70)
        
        # Find priority conversations
        logger.info("Searching for priority conversations...")
        priority_convs = await self.find_priority_conversations()
        
        if not priority_convs:
            logger.info("No priority conversations found")
            return
        
        logger.info(f"\nFound {len(priority_convs)} priority conversations")
        logger.info("Starting migration...\n")
        
        total_chunks = 0
        for conv in priority_convs:
            chunks = await self.migrate_conversation(conv)
            total_chunks += chunks
            await asyncio.sleep(0.5)  # Rate limiting
        
        logger.info("\n" + "=" * 70)
        logger.info("PRIORITY MIGRATION COMPLETE")
        logger.info("=" * 70)
        logger.info(f"Conversations migrated: {len(priority_convs)}")
        logger.info(f"Total v2 chunks created: {total_chunks}")
        logger.info("\n✅ Critical content is now searchable!")
        logger.info("Run full migration later for complete coverage.")


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
    migrator = PriorityV2Migrator()
    await migrator.run()


if __name__ == "__main__":
    asyncio.run(main())