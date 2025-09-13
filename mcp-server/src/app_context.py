"""Application context for sharing state across modules."""

from dataclasses import dataclass
from typing import Optional, Any
from qdrant_client import AsyncQdrantClient
try:
    from .embedding_manager import EmbeddingManager
    from .decay_manager import DecayManager
    from .utils import ProjectResolver
except ImportError:
    # Fallback for testing
    EmbeddingManager = None
    DecayManager = None
    ProjectResolver = None

@dataclass
class AppContext:
    """Shared application context for all MCP tools."""
    
    qdrant_client: AsyncQdrantClient
    embedding_manager: EmbeddingManager
    decay_manager: DecayManager
    project_resolver: ProjectResolver
    
    # Optional context for debugging
    debug_context: Optional[Any] = None
    
    def __post_init__(self):
        """Initialize any additional state after dataclass creation."""
        # Ensure all managers are properly initialized
        if not self.embedding_manager:
            self.embedding_manager = EmbeddingManager()
        
        if not self.decay_manager:
            self.decay_manager = DecayManager()
        
        if not self.project_resolver:
            self.project_resolver = ProjectResolver()
    
    async def get_all_collections(self) -> list:
        """Get all collections from Qdrant."""
        try:
            collections = await self.qdrant_client.get_collections()
            return [c.name for c in collections.collections]
        except Exception as e:
            if self.debug_context:
                await self.debug_context.debug(f"Failed to get collections: {e}")
            return []
    
    async def generate_embedding(self, text: str, embedding_type: Optional[str] = None):
        """Generate embedding using the embedding manager."""
        # The embedding_manager.embed method is synchronous, not async
        embeddings = self.embedding_manager.embed(text, input_type="document")
        if embeddings and len(embeddings) > 0:
            return embeddings[0]
        return None
    
    def get_current_project(self) -> Optional[str]:
        """Get current project from resolver."""
        return self.project_resolver.get_current_project()
    
    def normalize_project_name(self, project_name: str) -> str:
        """Normalize project name using resolver."""
        return self.project_resolver.normalize_project_name(project_name)