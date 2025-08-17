#!/usr/bin/env python3
"""
Background repair script to fix truncated chunks in existing collections.
Runs incrementally to avoid system overload while gradually migrating to v2 chunking.
"""

import asyncio
import json
import os
import time
from pathlib import Path
from typing import Dict, List, Optional, Any, Set
from datetime import datetime, timedelta
import logging
from dataclasses import dataclass

from qdrant_client import AsyncQdrantClient, models
from fastembed import TextEmbedding
# Removed langchain dependency for Docker compatibility

# Add parent directory to path for imports
import sys
sys.path.insert(0, str(Path(__file__).parent))
from utils import normalize_project_name

# Configure logging
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)


@dataclass
class RepairConfig:
    """Configuration for repair process."""
    qdrant_url: str = os.getenv("QDRANT_URL", "http://localhost:6333")
    repair_rate_per_minute: int = 10
    pause_hours: List[int] = None  # Hours to pause (e.g., [9, 10, 11])
    priority_projects: List[str] = None  # Projects to repair first
    chunk_size_tokens: int = 400
    chunk_overlap_tokens: int = 75
    embedding_model: str = "sentence-transformers/all-MiniLM-L6-v2"
    logs_dir: Path = Path("/logs")
    state_file: Path = Path("/config/repair-state.json")
    dry_run: bool = False
    
    def __post_init__(self):
        if self.pause_hours is None:
            self.pause_hours = [9, 10, 11, 14, 15, 16]  # Default peak hours
        if self.priority_projects is None:
            self.priority_projects = ["anukruti", "claude-self-reflect"]
        
        # Load from migration config if available
        self.load_migration_config()
    
    def load_migration_config(self):
        """Load settings from migration config if available."""
        config_path = Path("/config/migration-config.json")
        if not config_path.exists():
            config_path = Path(__file__).parent.parent / "config" / "migration-config.json"
        
        if config_path.exists():
            try:
                with open(config_path) as f:
                    config = json.load(f)
                    migration = config.get("migration", {})
                    self.repair_rate_per_minute = migration.get("repair_rate_per_minute", self.repair_rate_per_minute)
                    self.pause_hours = migration.get("pause_hours", self.pause_hours)
                    self.priority_projects = migration.get("priority_projects", self.priority_projects)
                    
                    chunking = config.get("chunking", {})
                    self.chunk_size_tokens = chunking.get("chunk_size_tokens", self.chunk_size_tokens)
                    self.chunk_overlap_tokens = chunking.get("chunk_overlap_tokens", self.chunk_overlap_tokens)
                    
                    logger.info(f"Loaded migration config from {config_path}")
            except Exception as e:
                logger.warning(f"Failed to load migration config: {e}")


class ChunkRepairer:
    """Repairs truncated chunks in existing Qdrant collections."""
    
    def __init__(self, config: RepairConfig):
        self.config = config
        self.client = AsyncQdrantClient(url=config.qdrant_url)
        self.embedding_model = TextEmbedding(config.embedding_model)
        self.state = self.load_state()
        self.stats = {
            "chunks_checked": 0,
            "chunks_repaired": 0,
            "chunks_skipped": 0,
            "errors": 0,
            "start_time": time.time()
        }
        
        # Initialize chunking parameters (no external dependencies)
        self.chunk_size_chars = config.chunk_size_tokens * 4  # Approximate chars
        self.chunk_overlap_chars = config.chunk_overlap_tokens * 4
    
    def load_state(self) -> Dict:
        """Load repair state from file."""
        if self.config.state_file.exists():
            try:
                with open(self.config.state_file) as f:
                    return json.load(f)
            except Exception as e:
                logger.error(f"Failed to load state: {e}")
        
        return {
            "repaired_points": {},  # collection -> set of point IDs
            "last_collection": None,
            "last_point_id": None,
            "total_repaired": 0
        }
    
    def save_state(self):
        """Save repair state to file."""
        try:
            self.config.state_file.parent.mkdir(parents=True, exist_ok=True)
            with open(self.config.state_file, 'w') as f:
                json.dump(self.state, f, indent=2, default=str)
        except Exception as e:
            logger.error(f"Failed to save state: {e}")
    
    def needs_repair(self, point: Any) -> bool:
        """Check if a point needs repair."""
        payload = point.payload
        
        # Skip if already v2
        if payload.get('chunking_version') == 'v2':
            return False
        
        # Check for truncation indicators
        text = payload.get('text', '')
        
        # Common truncation patterns
        if len(text) == 1500:  # Exact truncation length
            return True
        
        if text.endswith('[...]') or text.endswith('...'):  # Truncation markers
            return True
        
        if 'NO TEXT' in text or text == 'unknown: unknown:':  # Known bad patterns
            return True
        
        # Check if text seems incomplete (ends mid-sentence)
        if text and not text[-1] in '.!?"\')':
            # Might be truncated
            if len(text) > 1400:  # Near truncation limit
                return True
        
        # Check truncation metadata if available
        if payload.get('messages_truncated', 0) > 0:
            return True
        
        return False
    
    async def get_original_content(self, point: Any) -> Optional[str]:
        """Retrieve original content for a point from JSONL files."""
        payload = point.payload
        conversation_id = payload.get('conversation_id')
        project = payload.get('project')
        
        if not conversation_id or not project:
            return None
        
        # Try to find the original JSONL file
        # This assumes the logs directory structure
        possible_paths = [
            self.config.logs_dir / project / f"{conversation_id}.jsonl",
            self.config.logs_dir / f"{project}" / f"{conversation_id}.jsonl",
            # Handle dash-encoded paths
            Path("/logs") / project.replace('-', '/') / f"{conversation_id}.jsonl"
        ]
        
        for path in possible_paths:
            if path.exists():
                try:
                    messages = []
                    with open(path) as f:
                        for line in f:
                            try:
                                data = json.loads(line.strip())
                                # Handle different formats
                                if 'message' in data:
                                    messages.append(data['message'])
                                elif 'role' in data:
                                    messages.append(data)
                            except json.JSONDecodeError:
                                continue
                    
                    # Reconstruct the full text
                    full_text = "\n\n".join([
                        f"{msg.get('role', 'unknown')}: {self.extract_message_text(msg.get('content', ''))}"
                        for msg in messages
                    ])
                    
                    return full_text
                    
                except Exception as e:
                    logger.error(f"Failed to read {path}: {e}")
        
        return None
    
    def simple_chunk_text(self, text: str) -> List[str]:
        """
        Simple token-aware text chunking without external dependencies.
        """
        if not text or len(text) <= self.chunk_size_chars:
            return [text] if text else []
        
        chunks = []
        start = 0
        
        while start < len(text):
            # Determine chunk end
            end = min(start + self.chunk_size_chars, len(text))
            
            # If not at the end of text, try to break at a natural boundary
            if end < len(text):
                # Look for sentence boundaries first
                for separator in ['. ', '.\n', '! ', '? ', '\n\n', '\n', ' ']:
                    last_sep = text.rfind(separator, start, end)
                    if last_sep > start + (self.chunk_size_chars // 2):  # At least half chunk size
                        end = last_sep + len(separator)
                        break
            
            # Extract chunk
            chunk = text[start:end].strip()
            if chunk:
                chunks.append(chunk)
            
            # Move start position with overlap
            if end >= len(text):
                break
            start = max(start + 1, end - self.chunk_overlap_chars)
        
        return chunks
    
    def extract_message_text(self, content: Any) -> str:
        """Extract text from message content (handles array format)."""
        if content is None:
            return ""
        
        if isinstance(content, str):
            return content.strip()
        
        if isinstance(content, list):
            text_parts = []
            for item in content:
                if isinstance(item, str):
                    text_parts.append(item)
                elif isinstance(item, dict):
                    if item.get('type') == 'text':
                        text_parts.append(item.get('text', ''))
                    elif item.get('type') == 'tool_result':
                        tool_content = item.get('content', '')
                        text_parts.append(self.extract_message_text(tool_content))
            return ' '.join(filter(None, text_parts)).strip()
        
        if isinstance(content, dict):
            for field in ['text', 'content', 'message']:
                if field in content:
                    return self.extract_message_text(content[field])
        
        return str(content)[:1000]
    
    async def repair_point(self, collection_name: str, point: Any) -> bool:
        """Repair a single truncated point."""
        if self.config.dry_run:
            logger.info(f"[DRY RUN] Would repair point {point.id} in {collection_name}")
            return True
        
        # Get original content
        original_content = await self.get_original_content(point)
        
        if not original_content:
            logger.warning(f"Could not retrieve original content for point {point.id}")
            return False
        
        # Create new chunks using simple token-aware splitting
        chunks = self.simple_chunk_text(original_content)
        
        if not chunks:
            logger.warning(f"No chunks created for point {point.id}")
            return False
        
        # For now, update the existing point with the first chunk
        # In a full implementation, we might create multiple points
        chunk_text = chunks[0]
        
        # Generate new embedding
        embeddings = list(self.embedding_model.passage_embed([chunk_text]))
        
        if not embeddings:
            logger.error(f"Failed to generate embedding for point {point.id}")
            return False
        
        # Update the point
        try:
            # Update payload with v2 metadata
            new_payload = point.payload.copy()
            new_payload['text'] = chunk_text
            new_payload['chunking_version'] = 'v2'
            new_payload['chunk_method'] = 'token_aware'
            new_payload['repaired_at'] = datetime.now().isoformat()
            new_payload['original_length'] = len(original_content)
            
            # Update the point in Qdrant
            await self.client.upsert(
                collection_name=collection_name,
                points=[
                    models.PointStruct(
                        id=point.id,
                        vector=embeddings[0].tolist(),
                        payload=new_payload
                    )
                ]
            )
            
            logger.info(f"Repaired point {point.id} in {collection_name} "
                       f"(text: {len(point.payload.get('text', ''))} -> {len(chunk_text)} chars)")
            
            return True
            
        except Exception as e:
            logger.error(f"Failed to update point {point.id}: {e}")
            return False
    
    def should_pause(self) -> bool:
        """Check if we should pause based on time."""
        current_hour = datetime.now().hour
        return current_hour in self.config.pause_hours
    
    async def repair_collection(self, collection_name: str) -> int:
        """Repair truncated chunks in a collection."""
        repaired_count = 0
        
        # Get points in batches
        offset = None
        batch_size = 100
        
        while True:
            try:
                # Scroll through points
                response = await self.client.scroll(
                    collection_name=collection_name,
                    limit=batch_size,
                    offset=offset,
                    with_payload=True,
                    with_vectors=False  # Don't need vectors for checking
                )
                
                points, next_offset = response
                
                if not points:
                    break
                
                for point in points:
                    self.stats["chunks_checked"] += 1
                    
                    # Skip if already repaired
                    point_id = str(point.id)
                    if point_id in self.state.get("repaired_points", {}).get(collection_name, set()):
                        self.stats["chunks_skipped"] += 1
                        continue
                    
                    # Check if needs repair
                    if self.needs_repair(point):
                        # Rate limiting
                        if not self.config.dry_run:
                            sleep_time = 60.0 / self.config.repair_rate_per_minute
                            await asyncio.sleep(sleep_time)
                        
                        # Check for pause hours
                        if self.should_pause():
                            logger.info("Pausing during peak hours...")
                            while self.should_pause():
                                await asyncio.sleep(300)  # Check every 5 minutes
                        
                        # Repair the point
                        success = await self.repair_point(collection_name, point)
                        
                        if success:
                            repaired_count += 1
                            self.stats["chunks_repaired"] += 1
                            
                            # Update state
                            if collection_name not in self.state["repaired_points"]:
                                self.state["repaired_points"][collection_name] = set()
                            self.state["repaired_points"][collection_name].add(point_id)
                            self.state["total_repaired"] += 1
                            
                            # Save state periodically
                            if self.state["total_repaired"] % 10 == 0:
                                self.save_state()
                        else:
                            self.stats["errors"] += 1
                    else:
                        self.stats["chunks_skipped"] += 1
                
                # Update offset for next batch
                offset = next_offset
                if offset is None:
                    break
                    
            except Exception as e:
                logger.error(f"Error processing collection {collection_name}: {e}")
                break
        
        return repaired_count
    
    async def run(self):
        """Main repair loop."""
        logger.info("Starting chunk repair process...")
        logger.info(f"Config: rate={self.config.repair_rate_per_minute}/min, "
                   f"pause_hours={self.config.pause_hours}, dry_run={self.config.dry_run}")
        
        # Get all collections
        try:
            response = await self.client.get_collections()
            all_collections = [c.name for c in response.collections]
            
            # Filter for conversation collections
            collections = [c for c in all_collections if c.startswith("conv_")]
            
            # Sort by priority
            priority_collections = []
            other_collections = []
            
            for collection in collections:
                is_priority = any(
                    proj in collection 
                    for proj in self.config.priority_projects
                )
                if is_priority:
                    priority_collections.append(collection)
                else:
                    other_collections.append(collection)
            
            # Process priority collections first
            all_collections_sorted = priority_collections + other_collections
            
            logger.info(f"Found {len(collections)} conversation collections")
            logger.info(f"Priority collections: {priority_collections}")
            
            # Process each collection
            for collection_name in all_collections_sorted:
                logger.info(f"Processing collection: {collection_name}")
                
                repaired = await self.repair_collection(collection_name)
                
                logger.info(f"Repaired {repaired} chunks in {collection_name}")
                
                # Print stats
                elapsed = time.time() - self.stats["start_time"]
                rate = self.stats["chunks_repaired"] / (elapsed / 60) if elapsed > 0 else 0
                
                logger.info(f"Stats: checked={self.stats['chunks_checked']}, "
                           f"repaired={self.stats['chunks_repaired']}, "
                           f"skipped={self.stats['chunks_skipped']}, "
                           f"errors={self.stats['errors']}, "
                           f"rate={rate:.1f}/min")
            
            # Final save
            self.save_state()
            
            logger.info("Repair process complete!")
            logger.info(f"Total repaired: {self.state['total_repaired']}")
            
        except Exception as e:
            logger.error(f"Fatal error: {e}")
            raise


async def main():
    """Main entry point."""
    # Parse command line arguments
    import argparse
    parser = argparse.ArgumentParser(description="Repair truncated chunks in Qdrant collections")
    parser.add_argument("--dry-run", action="store_true", help="Simulate repair without making changes")
    parser.add_argument("--rate", type=int, default=10, help="Repair rate per minute")
    parser.add_argument("--projects", nargs="+", help="Priority projects to repair first")
    args = parser.parse_args()
    
    # Create config
    config = RepairConfig(
        dry_run=args.dry_run,
        repair_rate_per_minute=args.rate
    )
    
    if args.projects:
        config.priority_projects = args.projects
    
    # Run repairer
    repairer = ChunkRepairer(config)
    await repairer.run()


if __name__ == "__main__":
    asyncio.run(main())