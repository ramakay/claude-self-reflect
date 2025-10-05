"""Abstract base class for embedding providers."""

from abc import ABC, abstractmethod
from typing import List, Optional, Dict, Any
from ..core.exceptions import EmbeddingError


class EmbeddingProvider(ABC):
    """
    Abstract interface for embedding providers.
    
    Defines the contract that all embedding providers must implement,
    including error handling methods as recommended by code review.
    """
    
    def __init__(self):
        self._last_error: Optional[Exception] = None
        self._initialized: bool = False
    
    @abstractmethod
    def initialize(self, config: Any) -> None:
        """
        Initialize the embedding provider.
        
        Args:
            config: Configuration object with provider-specific settings
            
        Raises:
            EmbeddingError: If initialization fails
        """
        pass
    
    @abstractmethod
    def embed_texts(self, texts: List[str]) -> List[List[float]]:
        """
        Generate embeddings for a list of texts.
        
        Args:
            texts: List of text strings to embed
            
        Returns:
            List of embedding vectors
            
        Raises:
            EmbeddingError: If embedding generation fails
        """
        pass
    
    @abstractmethod
    def get_dimension(self) -> int:
        """
        Get the dimension of embeddings produced by this provider.
        
        Returns:
            Embedding dimension
        """
        pass
    
    @abstractmethod
    def validate_embedding(self, embedding: List[float]) -> bool:
        """
        Validate that an embedding is well-formed and not degenerate.
        
        Args:
            embedding: Embedding vector to validate
            
        Returns:
            True if embedding is valid, False otherwise
        """
        pass
    
    @abstractmethod
    def handle_initialization_error(self, error: Exception) -> None:
        """
        Handle initialization failures.
        
        Args:
            error: The exception that occurred during initialization
        """
        pass
    
    def get_last_error(self) -> Optional[Exception]:
        """
        Retrieve the last error for diagnostics.
        
        Returns:
            Last exception that occurred, or None
        """
        return self._last_error
    
    def is_initialized(self) -> bool:
        """
        Check if provider is initialized and ready.
        
        Returns:
            True if initialized, False otherwise
        """
        return self._initialized
    
    def get_provider_info(self) -> Dict[str, Any]:
        """
        Get information about this embedding provider.
        
        Returns:
            Dictionary with provider metadata
        """
        return {
            "provider": self.__class__.__name__,
            "initialized": self._initialized,
            "dimension": self.get_dimension() if self._initialized else None,
            "has_error": self._last_error is not None
        }
    
    def batch_embed_texts(
        self, 
        texts: List[str], 
        batch_size: int = 32
    ) -> List[List[float]]:
        """
        Generate embeddings in batches for memory efficiency.
        
        Args:
            texts: List of texts to embed
            batch_size: Number of texts to process at once
            
        Returns:
            List of embedding vectors
            
        Raises:
            EmbeddingError: If embedding generation fails
        """
        if not self._initialized:
            raise EmbeddingError("Provider not initialized", provider=self.__class__.__name__)
        
        embeddings = []
        for i in range(0, len(texts), batch_size):
            batch = texts[i:i + batch_size]
            batch_embeddings = self.embed_texts(batch)
            embeddings.extend(batch_embeddings)
        
        return embeddings