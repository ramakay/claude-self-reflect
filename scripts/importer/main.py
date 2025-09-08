"""Main orchestrator with dependency injection."""

import logging
import time
from pathlib import Path
from typing import List, Optional, Dict, Any
from dependency_injector import containers, providers

from .core import (
    ImportConfig,
    Message,
    ConversationChunk,
    ProcessedPoint,
    ImportResult,
    ImportStats
)
from .core.exceptions import ImportError, ParseError, ValidationError
from .embeddings import EmbeddingProvider, FastEmbedProvider
try:
    from .embeddings import VoyageEmbeddingProvider
    VOYAGE_AVAILABLE = True
except ImportError:
    VoyageEmbeddingProvider = None
    VOYAGE_AVAILABLE = False
from .processors import (
    ConversationParser,
    Chunker,
    ASTExtractor,
    ConceptExtractor,
    ToolUsageExtractor
)
from .storage import QdrantStorage
from .state import StateManager
from .utils import ProjectNormalizer, setup_logging

logger = logging.getLogger(__name__)


class ConversationProcessor:
    """
    Main orchestrator for processing conversations.
    
    Follows dependency injection pattern with all dependencies
    injected through constructor.
    """
    
    def __init__(
        self,
        config: ImportConfig,
        embedding_provider: EmbeddingProvider,
        storage: QdrantStorage,
        parser: ConversationParser,
        chunker: Chunker,
        extractors: List[Any],
        state_manager: StateManager,
        normalizer: ProjectNormalizer
    ):
        self.config = config
        self.embedding_provider = embedding_provider
        self.storage = storage
        self.parser = parser
        self.chunker = chunker
        self.extractors = extractors
        self.state = state_manager
        self.normalizer = normalizer
        self.stats = ImportStats()
    
    def process_file(self, file_path: Path) -> ImportResult:
        """
        Process a single JSONL file.
        
        Returns:
            ImportResult with processing details
        """
        start_time = time.time()
        result = ImportResult(file_path=str(file_path), success=False)
        
        try:
            # Check if already processed
            if not self.config.force_reimport and self.state.is_processed(file_path):
                logger.info(f"Skipping already processed: {file_path}")
                result.success = True
                return result
            
            # Parse conversation
            logger.debug(f"Parsing conversation: {file_path}")
            messages = self.parser.parse_file(file_path)
            if not messages:
                raise ParseError(str(file_path), reason="No messages found")
            
            # Create chunks
            logger.debug(f"Creating chunks for {len(messages)} messages")
            chunks = self.chunker.create_chunks(messages, str(file_path))
            result.chunks_processed = len(chunks)
            
            # Extract metadata
            logger.debug("Extracting metadata")
            self._enrich_chunks(chunks)
            
            # Generate embeddings
            logger.debug("Generating embeddings")
            texts = [chunk.text for chunk in chunks]
            # Use embed_batch for proper token-aware batching with Voyage
            if hasattr(self.embedding_provider, 'embed_batch'):
                embeddings = self.embedding_provider.embed_batch(texts)
            else:
                embeddings = self.embedding_provider.embed(texts)
            
            # Build points
            logger.debug("Building points")
            points = self._build_points(chunks, embeddings, file_path)
            
            # Store in Qdrant
            logger.debug(f"Storing {len(points)} points")
            collection_name = self._get_collection_name(file_path)
            stored = self.storage.upsert_points(collection_name, points)
            result.points_created = stored
            
            # Update state
            self.state.mark_processed(file_path, stored)
            
            result.success = True
            logger.info(f"Successfully processed {file_path}: {stored} points")
            
        except Exception as e:
            logger.error(f"Failed to process {file_path}: {e}")
            result.error = str(e)
            self.state.mark_failed(file_path, str(e))
            if not isinstance(e, ImportError):
                raise ImportError(f"Processing failed: {e}")
        
        finally:
            result.duration_seconds = time.time() - start_time
            self.stats.add_result(result)
        
        return result
    
    def _enrich_chunks(self, chunks: List[ConversationChunk]) -> None:
        """Add metadata to chunks using extractors."""
        for chunk in chunks:
            for extractor in self.extractors:
                try:
                    metadata = extractor.extract(chunk.text)
                    for key, value in metadata.items():
                        chunk.add_metadata(key, value)
                except Exception as e:
                    logger.warning(f"Extractor {extractor.__class__.__name__} failed: {e}")
    
    def _build_points(
        self,
        chunks: List[ConversationChunk],
        embeddings: List[List[float]],
        file_path: Path
    ) -> List[ProcessedPoint]:
        """Build Qdrant points from chunks and embeddings."""
        points = []
        project_name = self.normalizer.get_project_name(file_path)
        
        for chunk, embedding in zip(chunks, embeddings):
            # Generate unique point ID
            point_id = f"{project_name}_{chunk.unique_id}"
            
            # Build payload
            payload = {
                "text": chunk.text,
                "project": project_name,
                "file_path": str(file_path),
                "chunk_index": chunk.chunk_index,
                "total_chunks": chunk.total_chunks,
                "message_indices": chunk.message_indices,
                **chunk.metadata
            }
            
            point = ProcessedPoint(
                id=point_id,
                vector=embedding,
                payload=payload
            )
            
            # Validate dimension
            if not point.validate_dimension(self.embedding_provider.get_dimension()):
                raise ValidationError(
                    "embedding",
                    len(embedding),
                    f"Expected dimension {self.embedding_provider.get_dimension()}"
                )
            
            points.append(point)
        
        return points
    
    def _get_collection_name(self, file_path: Path) -> str:
        """Generate collection name for file."""
        return self.normalizer.get_collection_name(file_path)
    
    def get_stats(self) -> ImportStats:
        """Get import statistics."""
        return self.stats


class ImporterContainer(containers.DeclarativeContainer):
    """
    Dependency injection container using dependency-injector library.
    
    This provides sophisticated dependency management as recommended
    in the code review.
    """
    
    # Configuration provider
    config = providers.Singleton(ImportConfig.from_env)
    
    # Logging setup
    logger_setup = providers.Resource(
        setup_logging,
        level=config.provided.log_level
    )
    
    # Core services
    normalizer = providers.Singleton(ProjectNormalizer)
    
    state_manager = providers.Singleton(
        StateManager,
        state_file=config.provided.state_file_path
    )
    
    # Embedding provider with selector
    def get_embedding_provider(config_obj):
        """Factory function to select embedding provider based on config."""
        if config_obj.use_voyage and config_obj.voyage_api_key:
            if not VOYAGE_AVAILABLE:
                logger.warning("Voyage requested but not available, falling back to FastEmbed")
                return FastEmbedProvider()
            return VoyageEmbeddingProvider(
                api_key=config_obj.voyage_api_key,
                model_name="voyage-2"
            )
        return FastEmbedProvider()
    
    embedding_provider = providers.Factory(
        get_embedding_provider,
        config_obj=config
    )
    
    # Storage
    storage = providers.Singleton(
        QdrantStorage,
        url=config.provided.qdrant_url,
        api_key=config.provided.qdrant_api_key
    )
    
    # Processors
    parser = providers.Singleton(ConversationParser)
    
    chunker = providers.Singleton(
        Chunker,
        chunk_size=config.provided.chunk_size,
        chunk_overlap=config.provided.chunk_overlap
    )
    
    # Metadata extractors
    ast_extractor = providers.Singleton(
        ASTExtractor,
        max_elements=config.provided.max_ast_elements
    )
    
    concept_extractor = providers.Singleton(ConceptExtractor)
    
    tool_extractor = providers.Singleton(ToolUsageExtractor)
    
    extractors = providers.List(
        ast_extractor,
        concept_extractor,
        tool_extractor
    )
    
    # Main processor
    processor = providers.Factory(
        ConversationProcessor,
        config=config,
        embedding_provider=embedding_provider,
        storage=storage,
        parser=parser,
        chunker=chunker,
        extractors=extractors,
        state_manager=state_manager,
        normalizer=normalizer
    )


def create_processor(config: Optional[ImportConfig] = None) -> ConversationProcessor:
    """
    Factory function to create a configured processor.
    
    Args:
        config: Optional configuration, uses environment if not provided
        
    Returns:
        Configured ConversationProcessor instance
    """
    container = ImporterContainer()
    
    if config:
        container.config.override(config)
    
    # Get processor instance
    processor = container.processor()
    
    # Note: Providers are already initialized by the container
    # No need to call initialize methods
    
    return processor


def process_files(
    files: List[Path],
    config: Optional[ImportConfig] = None,
    progress_callback: Optional[Any] = None
) -> ImportStats:
    """
    Process multiple files with progress tracking.
    
    Args:
        files: List of JSONL files to process
        config: Optional configuration
        progress_callback: Optional callback for progress updates
        
    Returns:
        ImportStats with aggregate results
    """
    processor = create_processor(config)
    
    for i, file_path in enumerate(files):
        if progress_callback:
            progress_callback(i, len(files), file_path)
        
        try:
            result = processor.process_file(file_path)
            logger.info(
                f"[{i+1}/{len(files)}] Processed {file_path.name}: "
                f"{result.points_created} points"
            )
        except Exception as e:
            logger.error(f"Failed to process {file_path}: {e}")
    
    return processor.get_stats()


def main():
    """Main entry point for CLI execution."""
    import argparse
    
    parser = argparse.ArgumentParser(description="Import Claude conversations to Qdrant")
    parser.add_argument("--limit", type=int, help="Limit number of files to process")
    parser.add_argument("--dry-run", action="store_true", help="Dry run without importing")
    parser.add_argument("--force", action="store_true", help="Force reimport all files")
    parser.add_argument("--voyage", action="store_true", help="Use Voyage AI embeddings")
    parser.add_argument("--log-level", default="INFO", help="Logging level")
    
    args = parser.parse_args()
    
    # Setup logging
    setup_logging(args.log_level)
    
    # Create config from environment with CLI overrides
    config_dict = {}
    if args.dry_run:
        config_dict["dry_run"] = True
    if args.force:
        config_dict["force_reimport"] = True
    if args.voyage:
        config_dict["use_voyage"] = True
    if args.limit:
        config_dict["file_limit"] = args.limit
        
    config = ImportConfig.from_env()
    if config_dict:
        # Override with CLI args
        config = ImportConfig.from_dict({**config.__dict__, **config_dict})
    
    # Find all JSONL files
    base_path = Path.home() / ".claude" / "projects"
    files = list(base_path.glob("*/*.jsonl"))
    
    if args.limit:
        files = files[:args.limit]
    
    logger.info(f"Processing {len(files)} files...")
    
    # Process files
    stats = process_files(files, config)
    
    logger.info(f"Import complete: {stats}")
    return 0 if stats.failed_files == 0 else 1