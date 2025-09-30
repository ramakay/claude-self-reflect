#!/usr/bin/env python3
"""
Refactored import script with reduced complexity using modular components.
All functions have cyclomatic complexity < 10.
"""

import os
import sys
import gc
import argparse
import logging
import hashlib
import uuid
from pathlib import Path
from datetime import datetime
from typing import List, Dict, Any, Optional

# Add the scripts directory to the Python path
scripts_dir = Path(__file__).parent
sys.path.insert(0, str(scripts_dir))

# Import refactored components
from metadata_extractor import MetadataExtractor
from embedding_service import create_embedding_service
from import_strategies import StreamImportStrategy
from unified_state_manager import UnifiedStateManager

# Import Qdrant client
from qdrant_client import QdrantClient
from qdrant_client.models import PointStruct, Distance, VectorParams

# Import shared modules
sys.path.insert(0, str(Path(__file__).parent.parent))
try:
    from shared.normalization import normalize_project_name
except ImportError:
    try:
        from importer.utils.project_normalizer import normalize_project_name
        logging.debug("Using importer.utils.project_normalizer.normalize_project_name")
    except ImportError:
        from utils import normalize_project_name
        logging.warning("Using legacy utils.normalize_project_name")

# Set up logging
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

# Constants
QDRANT_URL = os.getenv("QDRANT_URL", "http://localhost:6333")
MAX_CHUNK_SIZE = int(os.getenv("MAX_CHUNK_SIZE", "50"))


class ConversationImporter:
    """Main class for importing conversations with reduced complexity."""

    def __init__(self):
        """Initialize the importer with all required services."""
        self.client = self._init_qdrant_client()
        self.embedding_service = create_embedding_service()
        self.state_manager = self._init_state_manager()
        self.metadata_extractor = MetadataExtractor()
        self.import_strategy = None

    def _init_qdrant_client(self) -> QdrantClient:
        """Initialize Qdrant client with optional authentication."""
        api_key = os.getenv("QDRANT_API_KEY")
        if api_key:
            return QdrantClient(url=QDRANT_URL, api_key=api_key, timeout=30)
        return QdrantClient(url=QDRANT_URL, timeout=30)

    def _init_state_manager(self) -> UnifiedStateManager:
        """Initialize state manager."""
        env_state = os.getenv("STATE_FILE")
        if env_state:
            state_file_path = Path(env_state).expanduser().resolve()
            return UnifiedStateManager(state_file_path)
        return UnifiedStateManager()

    def get_collection_name(self, project_path: Path) -> str:
        """Get collection name for a project."""
        project_name = normalize_project_name(str(project_path))
        suffix = self.embedding_service.get_collection_suffix()
        return f"csr_{project_name}_{suffix}"

    def ensure_collection(self, collection_name: str):
        """Ensure collection exists with correct configuration."""
        collections = self.client.get_collections().collections
        exists = any(c.name == collection_name for c in collections)

        if not exists:
            dimension = self.embedding_service.get_dimension()
            self.client.create_collection(
                collection_name=collection_name,
                vectors_config=VectorParams(size=dimension, distance=Distance.COSINE)
            )
            logger.info(f"Created collection: {collection_name} with {dimension} dimensions")

    def process_and_upload_chunk(
        self,
        messages: List[Dict[str, Any]],
        chunk_index: int,
        conversation_id: str,
        created_at: str,
        metadata: Dict[str, Any],
        collection_name: str,
        project_path: Path,
        total_messages: int
    ) -> int:
        """Process and upload a chunk of messages."""
        if not messages:
            return 0

        # Combine all message content into a single text for the chunk
        combined_text = "\n".join([msg['content'] for msg in messages])

        # Generate a single embedding for the entire chunk
        embeddings = self.embedding_service.generate_embeddings([combined_text])
        if not embeddings:
            return 0

        # Create points for upload
        points = self._create_points(
            messages, embeddings, chunk_index,
            conversation_id, created_at, metadata,
            project_path, total_messages
        )

        # Upload to Qdrant
        self._upload_points(collection_name, points)

        return 1  # Return number of chunks processed

    def _create_points(
        self,
        messages: List[Dict[str, Any]],
        embeddings: List[List[float]],
        chunk_index: int,
        conversation_id: str,
        created_at: str,
        metadata: Dict[str, Any],
        project_path: Path,
        total_messages: int
    ) -> List[PointStruct]:
        """Create Qdrant points from messages and embeddings."""
        points = []
        # Generate a proper UUID for the chunk ID
        # Use a deterministic UUID based on conversation_id and chunk_index for consistency
        chunk_string = f"{conversation_id}_chunk_{chunk_index}"
        chunk_uuid = str(uuid.uuid5(uuid.NAMESPACE_DNS, chunk_string))

        # Build conversation snippet
        snippet_parts = []
        for msg in messages[:5]:  # First 5 messages for snippet
            role = msg['role']
            content = msg['content'][:200]  # Truncate for snippet
            snippet_parts.append(f"{role}: {content}")
        conversation_snippet = "\n".join(snippet_parts)

        # Create point with proper vector format
        # Always use the first embedding for a chunk (combining messages into one embedding)
        point = PointStruct(
            id=chunk_uuid,
            vector=embeddings[0],
            payload={
                "conversation_id": conversation_id,
                "chunk_index": chunk_index,
                "created_at": created_at,
                "project": str(project_path),
                "messages": messages,
                "metadata": metadata,
                "conversation_snippet": conversation_snippet,
                "total_messages": total_messages,
                "embedding_model": self.embedding_service.get_provider_name()
            }
        )
        points.append(point)

        return points

    def _upload_points(self, collection_name: str, points: List[PointStruct]):
        """Upload points to Qdrant with retry logic."""
        max_retries = 3
        for attempt in range(max_retries):
            try:
                self.client.upsert(
                    collection_name=collection_name,
                    points=points,
                    wait=True
                )
                return
            except Exception as e:
                if attempt < max_retries - 1:
                    logger.warning(f"Upload attempt {attempt + 1} failed: {e}")
                else:
                    raise

    def should_import_file(self, file_path: Path) -> bool:
        """Check if a file should be imported."""
        if not file_path.exists() or file_path.stat().st_size == 0:
            return False

        # Check if file was already imported using UnifiedStateManager API
        imported_files = self.state_manager.get_imported_files()
        normalized_path = self.state_manager.normalize_path(str(file_path))

        # UnifiedStateManager returns files directly, not nested in 'files' key
        file_state = imported_files.get(normalized_path)
        if file_state:
            file_mtime = datetime.fromtimestamp(file_path.stat().st_mtime).replace(tzinfo=None)
            # Handle both old and new timestamp field names
            state_mtime_str = file_state.get('last_modified') or file_state.get('imported_at')
            if state_mtime_str:
                try:
                    state_mtime = datetime.fromisoformat(state_mtime_str).replace(tzinfo=None)
                    if file_mtime <= state_mtime:
                        logger.debug(f"Skipping {file_path.name} - already imported")
                        return False
                except ValueError:
                    logger.debug(f"Invalid timestamp in state for {file_path.name}; will re-import")

        return True

    def import_file(self, jsonl_file: Path, collection_name: str, project_path: Path) -> int:
        """Import a single JSONL file."""
        # Initialize import strategy if not already done
        if not self.import_strategy:
            self.import_strategy = StreamImportStrategy(
                self.client,
                self.process_and_upload_chunk,
                self.state_manager,
                MAX_CHUNK_SIZE
            )

        # Use strategy to import file
        chunks = self.import_strategy.import_file(jsonl_file, collection_name, project_path)

        # Update state if successful
        if chunks > 0:
            self.update_file_state(jsonl_file, chunks, collection_name)

        return chunks

    def update_file_state(self, file_path: Path, chunks: int, collection_name: str):
        """Update state for successfully imported file."""
        try:
            self.state_manager.add_imported_file(
                file_path=str(file_path),
                chunks=chunks,
                collection=collection_name,
                embedding_mode="local" if "Local" in self.embedding_service.get_provider_name() else "cloud"
            )
            logger.debug(f"Updated state for {file_path.name}")
        except Exception as e:
            logger.warning(f"Could not update state for {file_path}: {e}")

    def import_project(self, project_path: Path, limit: Optional[int] = None) -> Dict[str, Any]:
        """Import all conversations from a project."""
        collection_name = self.get_collection_name(project_path)
        self.ensure_collection(collection_name)

        # Find JSONL files
        jsonl_files = sorted(project_path.glob("*.jsonl"))
        if not jsonl_files:
            logger.warning(f"No JSONL files found in {project_path}")
            return {"imported": 0, "skipped": 0, "failed": 0}

        # Apply limit if specified
        if limit:
            jsonl_files = jsonl_files[:limit]

        # Import files
        stats = {"imported": 0, "skipped": 0, "failed": 0}

        for jsonl_file in jsonl_files:
            if not self.should_import_file(jsonl_file):
                stats["skipped"] += 1
                continue

            try:
                # Calculate expected chunks based on file size
                file_size = jsonl_file.stat().st_size
                expected_chunks = max(1, file_size // (1024 * 100))  # Rough estimate

                chunks = self.import_file(jsonl_file, collection_name, project_path)

                # Validate chunk count is reasonable
                if chunks > 0:
                    if chunks > expected_chunks * 10:
                        logger.warning(f"Unusual chunk count for {jsonl_file.name}: {chunks} chunks (expected ~{expected_chunks})")
                    stats["imported"] += 1
                else:
                    stats["failed"] += 1
            except Exception as e:
                logger.error(f"Failed to import {jsonl_file}: {e}")
                stats["failed"] += 1

            # Force garbage collection periodically
            if (stats["imported"] + stats["failed"]) % 10 == 0:
                gc.collect()

        return stats


def main():
    """Main entry point."""
    parser = argparse.ArgumentParser(description="Import conversations with reduced complexity")
    parser.add_argument("--project", type=str, help="Specific project path to import")
    parser.add_argument("--limit", type=int, help="Limit number of files to import")
    parser.add_argument("--verbose", action="store_true", help="Enable verbose logging")

    args = parser.parse_args()

    if args.verbose:
        logging.getLogger().setLevel(logging.DEBUG)

    # Create importer
    importer = ConversationImporter()

    # Determine project path
    if args.project:
        project_path = Path(args.project).expanduser().resolve()
        if not project_path.exists():
            logger.error(f"Project path does not exist: {project_path}")
            sys.exit(1)
        projects = [project_path]
    else:
        # Import all projects
        claude_dir = Path.home() / ".claude" / "projects"
        if not claude_dir.exists():
            logger.error(f"Claude projects directory not found: {claude_dir}")
            sys.exit(1)
        projects = [p for p in claude_dir.iterdir() if p.is_dir()]

    # Import projects
    total_stats = {"imported": 0, "skipped": 0, "failed": 0}

    for project in projects:
        logger.info(f"Importing project: {project.name}")
        stats = importer.import_project(project, args.limit)

        # Aggregate stats
        for key in total_stats:
            total_stats[key] += stats[key]

        logger.info(f"Project {project.name}: {stats}")

    # Print summary
    logger.info(f"\nImport complete:")
    logger.info(f"  Imported: {total_stats['imported']} conversations")
    logger.info(f"  Skipped: {total_stats['skipped']} conversations")
    logger.info(f"  Failed: {total_stats['failed']} conversations")


if __name__ == "__main__":
    main()