"""Voyage AI embedding provider with conditional import support."""

import logging
from typing import List, Optional

from .base import EmbeddingProvider

logger = logging.getLogger(__name__)

# Conditional import to avoid dependency when not using Voyage
try:
    import voyageai
    VOYAGE_AVAILABLE = True
except ImportError:
    voyageai = None
    VOYAGE_AVAILABLE = False
    logger.debug("Voyage AI not installed. Install with: pip install voyageai")


class VoyageEmbeddingProvider(EmbeddingProvider):
    """
    Voyage AI cloud embedding provider.
    
    Supports multiple models with different dimensions:
    - voyage-2: 1024 dimensions (default)
    - voyage-large-2: 1536 dimensions
    - voyage-3: 1024 dimensions
    - voyage-3-lite: 512 dimensions
    """
    
    SUPPORTED_MODELS = {
        "voyage-2": 1024,
        "voyage-large-2": 1536,
        "voyage-3": 1024,
        "voyage-3-lite": 512,
    }
    
    def __init__(
        self,
        api_key: str,
        model_name: str = "voyage-2",
        batch_size: int = 128,
        max_tokens_per_batch: int = 100000,  # 20k buffer from 120k limit
        token_estimation_ratio: int = 3  # chars per token
    ):
        """
        Initialize Voyage AI provider.
        
        Args:
            api_key: Voyage AI API key
            model_name: Model to use (default: voyage-2)
            batch_size: Maximum batch size for embedding
            max_tokens_per_batch: Maximum tokens per batch (default: 100000, 20k buffer from 120k limit)
            token_estimation_ratio: Characters per token estimate (default: 3)
            
        Raises:
            ImportError: If voyageai package is not installed
            ValueError: If API key is empty or model is unsupported
        """
        if not VOYAGE_AVAILABLE:
            raise ImportError(
                "Voyage AI is not installed. "
                "Install with: pip install voyageai"
            )
        
        if not api_key:
            raise ValueError("Voyage API key is required")
        
        if model_name not in self.SUPPORTED_MODELS:
            raise ValueError(
                f"Unsupported model: {model_name}. "
                f"Supported models: {list(self.SUPPORTED_MODELS.keys())}"
            )
        
        self.api_key = api_key
        self.model_name = model_name
        self.batch_size = batch_size
        self.dimension = self.SUPPORTED_MODELS[model_name]
        self.max_tokens_per_batch = max_tokens_per_batch
        self.token_estimation_ratio = token_estimation_ratio
        
        # Initialize client
        self.client = voyageai.Client(api_key=api_key)
        logger.info(f"Initialized Voyage AI with model {model_name} ({self.dimension} dims)")
        # Note: Never log the API key for security
    
    def embed(self, texts: List[str]) -> List[List[float]]:
        """
        Generate embeddings for texts using Voyage AI.
        
        Args:
            texts: List of texts to embed
            
        Returns:
            List of embedding vectors
            
        Raises:
            ValueError: If request is invalid
            Exception: If API call fails
        """
        if not texts:
            return []
        
        try:
            # Voyage AI expects a list of texts
            result = self.client.embed(
                texts=texts,
                model=self.model_name
            )
            
            # Extract embeddings from response
            embeddings = result.embeddings
            
            # Validate dimensions
            for i, embedding in enumerate(embeddings):
                if len(embedding) != self.dimension:
                    raise ValueError(
                        f"Embedding {i} has wrong dimension: "
                        f"expected {self.dimension}, got {len(embedding)}"
                    )
            
            logger.debug(f"Generated {len(embeddings)} embeddings with Voyage AI")
            return embeddings
            
        except AttributeError as e:
            # Handle voyageai-specific errors if available
            if 'RateLimitError' in str(type(e).__name__):
                logger.error(f"Rate limit exceeded, retry with backoff")
                raise
            elif 'InvalidRequestError' in str(type(e).__name__):
                logger.error(f"Invalid request to Voyage API")
                raise ValueError(f"Invalid embedding request: {e}")
            else:
                logger.error(f"Voyage API error: {e}")
                raise
        except Exception as e:
            logger.error(f"Unexpected error during embedding: {type(e).__name__}")
            raise
    
    def estimate_tokens(self, text: str) -> int:
        """
        Estimate token count for text.
        Conservative estimate: 3 characters = 1 token.
        
        Args:
            text: Text to estimate tokens for
            
        Returns:
            Estimated token count
        """
        return len(text) // self.token_estimation_ratio
    
    def embed_batch(self, texts: List[str]) -> List[List[float]]:
        """
        Generate embeddings in token-aware batches to respect API limits.
        
        This implements the critical fix for issue #38 - prevents
        "max allowed tokens per batch is 120000" errors.
        
        Args:
            texts: List of texts to embed
            
        Returns:
            List of embedding vectors
        """
        if not texts:
            return []
        
        all_embeddings = []
        current_batch = []
        current_tokens = 0
        
        for text in texts:
            # Estimate tokens for this text
            text_tokens = self.estimate_tokens(text)
            
            # Check if single text exceeds limit
            if text_tokens > self.max_tokens_per_batch:
                logger.warning(
                    f"Single text with {text_tokens} estimated tokens exceeds "
                    f"limit of {self.max_tokens_per_batch}. Truncating."
                )
                # Truncate text to fit within limit
                max_chars = self.max_tokens_per_batch * self.token_estimation_ratio
                text = text[:max_chars]
                text_tokens = self.estimate_tokens(text)
            
            # Check if adding this text would exceed batch limit
            if current_batch and (current_tokens + text_tokens) > self.max_tokens_per_batch:
                # Process current batch
                logger.debug(
                    f"Processing batch with {len(current_batch)} texts, "
                    f"~{current_tokens} tokens"
                )
                embeddings = self.embed(current_batch)
                all_embeddings.extend(embeddings)
                
                # Start new batch
                current_batch = [text]
                current_tokens = text_tokens
            else:
                # Add to current batch
                current_batch.append(text)
                current_tokens += text_tokens
        
        # Process final batch
        if current_batch:
            logger.debug(
                f"Processing final batch with {len(current_batch)} texts, "
                f"~{current_tokens} tokens"
            )
            embeddings = self.embed(current_batch)
            all_embeddings.extend(embeddings)
        
        return all_embeddings
    
    def get_dimension(self) -> int:
        """Get embedding dimension for current model."""
        return self.dimension
    
    def get_model_name(self) -> str:
        """Get the model name being used."""
        return self.model_name
    
    def validate_api_key(self) -> bool:
        """
        Validate that the API key works.
        
        Returns:
            True if API key is valid
        """
        try:
            # Test with a simple embedding
            test_result = self.client.embed(
                texts=["test"],
                model=self.model_name
            )
            return len(test_result.embeddings) > 0
        except Exception as e:
            logger.error(f"API key validation failed: {e}")
            return False
    
    @classmethod
    def is_available(cls) -> bool:
        """Check if Voyage AI is available for use."""
        return VOYAGE_AVAILABLE
    
    @classmethod
    def get_supported_models(cls) -> dict:
        """Get dictionary of supported models and their dimensions."""
        return cls.SUPPORTED_MODELS.copy()