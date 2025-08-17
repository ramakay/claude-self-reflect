#!/usr/bin/env python3
"""
Force V2 Migration Script
Actively converts priority conversations to v2 chunking.
Run this AFTER deploying v2.5.16 to immediately fix critical content.
"""

import asyncio
import json
import os
from pathlib import Path
from typing import List, Dict, Any
from datetime import datetime
import logging
import sys

sys.path.insert(0, str(Path(__file__).parent))
from utils import normalize_project_name

logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

class ForceV2Migration:
    """Force migration of critical conversations to v2 chunking."""
    
    def __init__(self):
        self.logs_dir = Path(os.path.expanduser("~/.claude/projects"))
        self.state_file = Path("config/force-migration-state.json")
        self.state = self.load_state()
        self.stats = {
            "files_processed": 0,
            "chunks_created": 0,
            "errors": 0
        }
    
    def load_state(self) -> Dict:
        """Load migration state."""
        if self.state_file.exists():
            with open(self.state_file) as f:
                return json.load(f)
        return {"migrated_files": []}
    
    def save_state(self):
        """Save migration state."""
        self.state_file.parent.mkdir(parents=True, exist_ok=True)
        with open(self.state_file, 'w') as f:
            json.dump(self.state, f, indent=2)
    
    def find_priority_files(self) -> List[Path]:
        """Find priority files to migrate."""
        priority_files = []
        
        # Priority 1: Files with known critical content
        critical_terms = ["TensorZero", "Sanskrit", "Observable", "A/B testing"]
        
        # Priority 2: Large files (likely to have truncation issues)
        size_threshold = 500_000  # 500KB
        
        # Priority 3: Recent files from key projects
        key_projects = ["claude-self-reflect", "anukruti", "ShopifyMCPMockShop"]
        
        logger.info(f"Scanning {self.logs_dir} for priority files...")
        
        for project_dir in self.logs_dir.iterdir():
            if not project_dir.is_dir():
                continue
            
            project_name = project_dir.name.lower()
            is_key_project = any(key in project_name for key in key_projects)
            
            for jsonl_file in project_dir.glob("*.jsonl"):
                # Skip if already migrated
                if str(jsonl_file) in self.state["migrated_files"]:
                    continue
                
                # Check file size
                file_size = jsonl_file.stat().st_size
                is_large = file_size > size_threshold
                
                # Check for critical content (quick scan)
                has_critical = False
                if file_size < 10_000_000:  # Only scan files < 10MB
                    try:
                        content = jsonl_file.read_text(errors='ignore')
                        has_critical = any(term in content for term in critical_terms)
                    except:
                        pass
                
                # Prioritize based on criteria
                if has_critical:
                    priority_files.insert(0, jsonl_file)  # Highest priority
                    logger.info(f"  Priority 1 (critical content): {jsonl_file.name}")
                elif is_large and is_key_project:
                    priority_files.append(jsonl_file)
                    logger.info(f"  Priority 2 (large + key project): {jsonl_file.name}")
                elif is_key_project:
                    priority_files.append(jsonl_file)
                    logger.debug(f"  Priority 3 (key project): {jsonl_file.name}")
        
        return priority_files[:20]  # Limit to top 20 for initial migration
    
    async def migrate_file(self, file_path: Path) -> bool:
        """Migrate a single file to v2 chunking."""
        logger.info(f"Migrating: {file_path.name}")
        
        try:
            # Import necessary modules
            from fastembed import TextEmbedding
            from qdrant_client import AsyncQdrantClient, models
            import hashlib
            
            # Initialize
            client = AsyncQdrantClient(url=os.getenv("QDRANT_URL", "http://localhost:6333"))
            embedding_model = TextEmbedding("sentence-transformers/all-MiniLM-L6-v2")
            
            # Get collection name
            project_path = str(file_path.parent)
            normalized = normalize_project_name(project_path)
            project_hash = hashlib.md5(normalized.encode()).hexdigest()[:8]
            collection_name = f"conv_{project_hash}_local"
            
            # Ensure collection exists
            try:
                await client.get_collection(collection_name)
            except:
                await client.create_collection(
                    collection_name=collection_name,
                    vectors_config=models.VectorParams(
                        size=384,
                        distance=models.Distance.COSINE
                    )
                )
            
            # Read and process file
            messages = []
            with open(file_path) as f:
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
                logger.warning(f"  No messages found in {file_path.name}")
                return False
            
            # Create v2 chunks using simple implementation
            class SimpleChunker:
                def __init__(self, chunk_size_tokens=400, chunk_overlap_tokens=75):
                    self.chunk_size_chars = chunk_size_tokens * 4
                    self.chunk_overlap_chars = chunk_overlap_tokens * 4
                
                def chunk_text(self, text):
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
            
            chunker = SimpleChunker(chunk_size_tokens=400, chunk_overlap_tokens=75)
            
            # Combine messages into text
            combined_text = "\n\n".join([
                f"{msg.get('role', 'unknown')}: {self.extract_text(msg.get('content', ''))}"
                for msg in messages
            ])
            
            # Create chunks
            chunks = chunker.chunk_text(combined_text)
            
            if not chunks:
                logger.warning(f"  No chunks created for {file_path.name}")
                return False
            
            # Generate embeddings and store
            conversation_id = file_path.stem
            points = []
            
            for idx, chunk_text in enumerate(chunks):
                # Generate embedding
                embeddings = list(embedding_model.embed([chunk_text]))
                if not embeddings:
                    continue
                
                # Create point
                point_id = hashlib.sha256(f"{conversation_id}_{idx}_v2".encode()).hexdigest()[:32]
                
                points.append(models.PointStruct(
                    id=point_id,
                    vector=embeddings[0].tolist(),
                    payload={
                        "text": chunk_text,
                        "conversation_id": conversation_id,
                        "chunk_index": idx,
                        "project": normalized,
                        "timestamp": datetime.now().isoformat(),
                        "chunking_version": "v2",
                        "chunk_method": "token_aware",
                        "chunk_overlap": True,
                        "migration_type": "forced",
                        "original_file": str(file_path)
                    }
                ))
            
            # Store in Qdrant
            if points:
                await client.upsert(
                    collection_name=collection_name,
                    points=points,
                    wait=True
                )
                
                logger.info(f"  ✅ Migrated {len(points)} v2 chunks to {collection_name}")
                self.stats["files_processed"] += 1
                self.stats["chunks_created"] += len(points)
                
                # Update state
                self.state["migrated_files"].append(str(file_path))
                self.save_state()
                
                return True
            
        except Exception as e:
            logger.error(f"  ❌ Error migrating {file_path.name}: {e}")
            self.stats["errors"] += 1
            return False
        
        return False
    
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
    
    async def run(self, limit: int = 10):
        """Run forced migration."""
        logger.info("=" * 60)
        logger.info("FORCE V2 MIGRATION STARTING")
        logger.info("=" * 60)
        
        # Find priority files
        priority_files = self.find_priority_files()
        
        if not priority_files:
            logger.info("No priority files found to migrate")
            return
        
        logger.info(f"\nFound {len(priority_files)} priority files")
        logger.info(f"Migrating top {min(limit, len(priority_files))} files...\n")
        
        # Migrate files
        for file_path in priority_files[:limit]:
            await self.migrate_file(file_path)
            await asyncio.sleep(1)  # Rate limiting
        
        # Report results
        logger.info("\n" + "=" * 60)
        logger.info("MIGRATION COMPLETE")
        logger.info("=" * 60)
        logger.info(f"Files processed: {self.stats['files_processed']}")
        logger.info(f"V2 chunks created: {self.stats['chunks_created']}")
        logger.info(f"Errors: {self.stats['errors']}")
        
        if self.stats['chunks_created'] > 0:
            logger.info("\n✅ Success! Critical content has been migrated to v2.")
            logger.info("Test searches for: TensorZero, Sanskrit, Observable")

async def main():
    """Main entry point."""
    import argparse
    parser = argparse.ArgumentParser(description="Force migration of priority conversations to v2 chunking")
    parser.add_argument("--limit", type=int, default=10, help="Number of files to migrate")
    parser.add_argument("--dry-run", action="store_true", help="Show what would be migrated without doing it")
    args = parser.parse_args()
    
    migrator = ForceV2Migration()
    
    if args.dry_run:
        logger.info("DRY RUN - Showing priority files:")
        files = migrator.find_priority_files()
        for i, f in enumerate(files[:args.limit], 1):
            print(f"{i}. {f.name} ({f.stat().st_size / 1024:.1f}KB)")
    else:
        await migrator.run(limit=args.limit)

if __name__ == "__main__":
    asyncio.run(main())