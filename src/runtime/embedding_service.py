"""
Embedding service abstraction to handle both local and cloud embeddings.
Reduces complexity by separating embedding concerns from import logic.
"""

import os
import logging
from abc import ABC, abstractmethod
from typing import List, Optional
from pathlib import Path

logger = logging.getLogger(__name__)


class EmbeddingProvider(ABC):
    """Abstract base class for embedding providers."""

    @abstractmethod
    def generate_embeddings(self, texts: List[str]) -> List[List[float]]:
        """Generate embeddings for a list of texts."""
        pass

    @abstractmethod
    def get_dimension(self) -> int:
        """Get the dimension of embeddings produced by this provider."""
        pass

    @abstractmethod
    def get_collection_suffix(self) -> str:
        """Get the suffix for collection naming."""
        pass


class LocalEmbeddingProvider(EmbeddingProvider):
    """Local embedding provider using FastEmbed."""

    def __init__(self):
        self.model = None
        self.dimension = 384
        self._initialize_model()

    def _initialize_model(self):
        """Initialize the FastEmbed model."""
        try:
            from fastembed import TextEmbedding
            # CRITICAL: Use the correct model that matches the rest of the system
            # This must be sentence-transformers/all-MiniLM-L6-v2 (384 dimensions)
            self.model = TextEmbedding(model_name="sentence-transformers/all-MiniLM-L6-v2")
            logger.info("Initialized local FastEmbed model: sentence-transformers/all-MiniLM-L6-v2 (384 dimensions)")
        except ImportError as e:
            logger.error("FastEmbed not installed. Install with: pip install fastembed")
            raise
        except Exception as e:
            logger.exception(f"Failed to initialize FastEmbed: {e}")
            raise

    def generate_embeddings(self, texts: List[str]) -> List[List[float]]:
        """Generate embeddings using FastEmbed."""
        if not self.model:
            raise RuntimeError("FastEmbed model not initialized")

        try:
            embeddings = list(self.model.embed(texts))
            return [list(emb) for emb in embeddings]
        except Exception as e:
            logger.error(f"Failed to generate local embeddings: {e}")
            raise

    def get_dimension(self) -> int:
        """Get embedding dimension (384 for FastEmbed)."""
        return self.dimension

    def get_collection_suffix(self) -> str:
        """Get collection suffix for local embeddings."""
        return "local_384d"


class CloudEmbeddingProvider(EmbeddingProvider):
    """Cloud embedding provider using Voyage AI."""

    def __init__(self, api_key: str):
        # Don't store API key directly, use it only for client initialization
        self.client = None
        self.dimension = 1024
        self._initialize_client(api_key)

    def _initialize_client(self, api_key: str):
        """Initialize the Voyage AI client."""
        try:
            import voyageai
            self.client = voyageai.Client(api_key=api_key)
            logger.info("Initialized Voyage AI client (1024 dimensions)")
        except ImportError as e:
            logger.error("voyageai not installed. Install with: pip install voyageai")
            raise
        except Exception as e:
            logger.exception(f"Failed to initialize Voyage AI: {e}")
            raise

    def generate_embeddings(self, texts: List[str]) -> List[List[float]]:
        """Generate embeddings using Voyage AI."""
        if not self.client:
            raise RuntimeError("Voyage AI client not initialized")

        try:
            result = self.client.embed(texts, model="voyage-2")
            return result.embeddings
        except Exception as e:
            logger.error(f"Failed to generate cloud embeddings: {e}")
            raise

    def get_dimension(self) -> int:
        """Get embedding dimension (1024 for Voyage)."""
        return self.dimension

    def get_collection_suffix(self) -> str:
        """Get collection suffix for cloud embeddings."""
        return "cloud_1024d"


class EmbeddingService:
    """
    Service to manage embedding generation with automatic provider selection.
    Reduces complexity by encapsulating embedding logic.
    """

    def __init__(self, prefer_local: bool = True, voyage_api_key: Optional[str] = None):
        """
        Initialize embedding service.

        Args:
            prefer_local: Whether to prefer local embeddings when available
            voyage_api_key: API key for Voyage AI (if using cloud embeddings)
        """
        self.prefer_local = prefer_local
        self.voyage_api_key = voyage_api_key
        self.provider = None
        self._initialize_provider()

    def _initialize_provider(self):
        """Initialize the appropriate embedding provider."""
        if self.prefer_local or not self.voyage_api_key:
            try:
                self.provider = LocalEmbeddingProvider()
                logger.info("Using local embedding provider (FastEmbed)")
            except Exception as e:
                logger.warning(f"Failed to initialize local provider: {e}")
                if self.voyage_api_key:
                    self._fallback_to_cloud()
                else:
                    raise RuntimeError("No embedding provider available")
        else:
            try:
                self.provider = CloudEmbeddingProvider(self.voyage_api_key)
                logger.info("Using cloud embedding provider (Voyage AI)")
            except Exception as e:
                logger.warning(f"Failed to initialize cloud provider: {e}")
                self._fallback_to_local()

    def _fallback_to_cloud(self):
        """Fallback to cloud provider."""
        if not self.voyage_api_key:
            raise RuntimeError("No Voyage API key available for cloud fallback")
        try:
            self.provider = CloudEmbeddingProvider(self.voyage_api_key)
            logger.info("Fallback to cloud embedding provider")
            # Clear the key after use
            self.voyage_api_key = None
        except Exception as e:
            raise RuntimeError(f"Failed to initialize any embedding provider: {e}")

    def _fallback_to_local(self):
        """Fallback to local provider."""
        try:
            self.provider = LocalEmbeddingProvider()
            logger.info("Fallback to local embedding provider")
        except Exception as e:
            raise RuntimeError(f"Failed to initialize any embedding provider: {e}")

    def generate_embeddings(self, texts: List[str]) -> List[List[float]]:
        """
        Generate embeddings for texts using the configured provider.

        Args:
            texts: List of texts to embed

        Returns:
            List of embedding vectors
        """
        if not self.provider:
            raise RuntimeError("No embedding provider initialized")

        # Filter out empty texts
        non_empty_texts = [t for t in texts if t and t.strip()]
        if not non_empty_texts:
            return []

        return self.provider.generate_embeddings(non_empty_texts)

    def get_dimension(self) -> int:
        """Get the dimension of embeddings."""
        if not self.provider:
            raise RuntimeError("No embedding provider initialized")
        return self.provider.get_dimension()

    def get_collection_suffix(self) -> str:
        """Get the collection suffix for current provider."""
        if not self.provider:
            raise RuntimeError("No embedding provider initialized")
        return self.provider.get_collection_suffix()

    def get_provider_name(self) -> str:
        """Get the name of the current provider."""
        if isinstance(self.provider, LocalEmbeddingProvider):
            return "FastEmbed (Local)"
        elif isinstance(self.provider, CloudEmbeddingProvider):
            return "Voyage AI (Cloud)"
        else:
            return "Unknown"


# Factory function for convenience
def create_embedding_service(
    prefer_local: Optional[bool] = None,
    voyage_api_key: Optional[str] = None
) -> EmbeddingService:
    """
    Create an embedding service with environment variable defaults.

    Args:
        prefer_local: Override for PREFER_LOCAL_EMBEDDINGS env var
        voyage_api_key: Override for VOYAGE_KEY env var

    Returns:
        Configured EmbeddingService instance
    """
    if prefer_local is None:
        prefer_local = os.getenv("PREFER_LOCAL_EMBEDDINGS", "true").lower() == "true"

    if voyage_api_key is None:
        voyage_api_key = os.getenv("VOYAGE_KEY")

    return EmbeddingService(prefer_local, voyage_api_key)