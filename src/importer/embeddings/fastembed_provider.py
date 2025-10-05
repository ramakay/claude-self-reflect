"""FastEmbed provider for local embeddings."""

from typing import List, Any
import logging
import statistics
from .base import EmbeddingProvider
from ..core.exceptions import EmbeddingError

logger = logging.getLogger(__name__)


class FastEmbedProvider(EmbeddingProvider):
    """
    FastEmbed provider for generating embeddings locally.
    
    Uses sentence-transformers/all-MiniLM-L6-v2 model by default.
    """
    
    def __init__(self):
        super().__init__()
        self.model = None
        self.model_name = None
        self.dimension = None
    
    def initialize(self, config: Any) -> None:
        """Initialize FastEmbed with the specified model."""
        try:
            from fastembed import TextEmbedding
            
            # CRITICAL: Use the correct model name
            self.model_name = config.embedding_model
            
            # FastEmbed uses specific model names
            if self.model_name == "sentence-transformers/all-MiniLM-L6-v2":
                # This is the correct model we must use
                fastembed_model = "sentence-transformers/all-MiniLM-L6-v2"
            else:
                fastembed_model = self.model_name
            
            logger.info(f"Initializing FastEmbed with model: {fastembed_model}")
            
            self.model = TextEmbedding(model_name=fastembed_model)
            self.dimension = config.embedding_dimension
            self._initialized = True
            
            logger.info(f"FastEmbed initialized successfully with dimension {self.dimension}")
            
        except ImportError as e:
            error = EmbeddingError(
                "FastEmbed not installed. Install with: pip install fastembed",
                provider="FastEmbed"
            )
            self.handle_initialization_error(error)
            raise error
        except Exception as e:
            error = EmbeddingError(
                f"Failed to initialize FastEmbed: {str(e)}",
                provider="FastEmbed"
            )
            self.handle_initialization_error(error)
            raise error
    
    def embed(self, texts: List[str]) -> List[List[float]]:
        """Generate embeddings for texts using FastEmbed."""
        if not self._initialized:
            raise EmbeddingError("FastEmbed not initialized", provider="FastEmbed")
        
        try:
            # FastEmbed returns a generator, convert to list
            embeddings = list(self.model.embed(texts))
            
            # Convert to regular Python lists with safe indexing
            result = []
            for i, embedding in enumerate(embeddings):
                # Convert numpy array or similar to list
                if hasattr(embedding, 'tolist'):
                    emb_list = embedding.tolist()
                else:
                    emb_list = list(embedding)
                
                # Validate each embedding
                if not self.validate_embedding(emb_list):
                    # Safe indexing - use i which is guaranteed to be valid
                    text_len = len(texts[i]) if i < len(texts) else 0
                    raise EmbeddingError(
                        f"Invalid embedding generated for text {i} of length {text_len}",
                        provider="FastEmbed"
                    )
                
                result.append(emb_list)
            
            return result
            
        except Exception as e:
            if not isinstance(e, EmbeddingError):
                e = EmbeddingError(
                    f"Failed to generate embeddings: {str(e)}",
                    provider="FastEmbed"
                )
            self._last_error = e
            raise e
    
    def get_dimension(self) -> int:
        """Get embedding dimension."""
        if not self._initialized:
            raise EmbeddingError("FastEmbed not initialized", provider="FastEmbed")
        return self.dimension
    
    def validate_embedding(self, embedding: List[float]) -> bool:
        """
        Validate embedding quality.
        
        Checks:
        1. Non-empty
        2. Correct dimension
        3. Not degenerate (all same values)
        4. Has reasonable variance
        """
        if not embedding:
            logger.error("Empty embedding detected")
            return False
        
        # Check dimension
        if len(embedding) != self.dimension:
            logger.error(
                f"Dimension mismatch: expected {self.dimension}, got {len(embedding)}"
            )
            return False
        
        # Check for degenerate embedding (all values identical)
        unique_values = len(set(embedding))
        if unique_values == 1:
            logger.error(f"Degenerate embedding detected (all values are {embedding[0]})")
            return False
        
        # Check variance is above threshold
        try:
            variance = statistics.variance(embedding)
            if variance < 1e-6:
                logger.warning(f"Low variance embedding detected: {variance}")
                # Don't fail on low variance, just warn
        except statistics.StatisticsError:
            # Less than 2 data points
            pass
        
        # Check for NaN or Inf values
        if any(not isinstance(x, (int, float)) or x != x or abs(x) == float('inf') 
               for x in embedding):
            logger.error("Embedding contains NaN or Inf values")
            return False
        
        return True
    
    def handle_initialization_error(self, error: Exception) -> None:
        """Handle and log initialization errors."""
        self._last_error = error
        self._initialized = False
        logger.error(f"FastEmbed initialization failed: {error}")
        
        # Could implement retry logic or fallback here
        if "not installed" in str(error):
            logger.info("Try: pip install fastembed")
        elif "model" in str(error).lower():
            logger.info(f"Model {self.model_name} may need to be downloaded first")