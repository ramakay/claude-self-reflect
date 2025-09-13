"""Embedding generation module for Claude Self-Reflect MCP server."""

import os
import voyageai
from typing import Dict, List, Optional, Any
from fastembed import TextEmbedding
from config import (
    VOYAGE_API_KEY, 
    VOYAGE_MODEL, 
    LOCAL_MODEL,
    PREFER_LOCAL_EMBEDDINGS,
    logger
)

class EmbeddingManager:
    """Manages embedding generation for both local and Voyage AI models."""
    
    def __init__(self):
        self.local_model = None
        self.voyage_client = None
        self.embedding_cache = {}
        
        # Initialize based on preference
        if PREFER_LOCAL_EMBEDDINGS or not VOYAGE_API_KEY:
            self._init_local_model()
        
        if VOYAGE_API_KEY:
            self._init_voyage_client()
    
    def _init_local_model(self):
        """Initialize local FastEmbed model."""
        try:
            self.local_model = TextEmbedding(
                model_name=LOCAL_MODEL,
                cache_dir=str(os.path.expanduser("~/.cache/fastembed"))
            )
            logger.info(f"Initialized local embedding model: {LOCAL_MODEL}")
        except Exception as e:
            logger.error(f"Failed to initialize local model: {e}")
    
    def _init_voyage_client(self):
        """Initialize Voyage AI client."""
        try:
            self.voyage_client = voyageai.Client(api_key=VOYAGE_API_KEY)
            logger.info("Initialized Voyage AI client")
        except Exception as e:
            logger.error(f"Failed to initialize Voyage client: {e}")
    
    async def generate_embedding(
        self, 
        text: str, 
        embedding_type: Optional[str] = None
    ) -> Optional[List[float]]:
        """Generate embedding for text using specified or default model."""
        
        # Use cache if available
        cache_key = f"{embedding_type or 'default'}:{text[:100]}"
        if cache_key in self.embedding_cache:
            return self.embedding_cache[cache_key]
        
        # Determine which model to use
        use_local = True
        if embedding_type:
            use_local = 'local' in embedding_type
        elif not PREFER_LOCAL_EMBEDDINGS and self.voyage_client:
            use_local = False
        
        try:
            if use_local and self.local_model:
                # Generate local embedding
                embeddings = list(self.local_model.embed([text]))
                if embeddings:
                    embedding = list(embeddings[0])
                    self.embedding_cache[cache_key] = embedding
                    return embedding
            
            elif self.voyage_client:
                # Generate Voyage embedding
                result = self.voyage_client.embed(
                    [text], 
                    model=VOYAGE_MODEL,
                    input_type="document"
                )
                if result.embeddings:
                    embedding = result.embeddings[0]
                    self.embedding_cache[cache_key] = embedding
                    return embedding
            
        except Exception as e:
            logger.error(f"Failed to generate embedding: {e}")
        
        return None
    
    async def generate_embeddings_batch(
        self, 
        texts: List[str], 
        embedding_type: Optional[str] = None
    ) -> Dict[str, List[float]]:
        """Generate embeddings for multiple texts efficiently."""
        results = {}
        
        # Determine which model to use
        use_local = True
        if embedding_type:
            use_local = 'local' in embedding_type
        elif not PREFER_LOCAL_EMBEDDINGS and self.voyage_client:
            use_local = False
        
        try:
            if use_local and self.local_model:
                # Batch process with local model
                embeddings = list(self.local_model.embed(texts))
                for text, embedding in zip(texts, embeddings):
                    results[text] = list(embedding)
            
            elif self.voyage_client:
                # Batch process with Voyage
                result = self.voyage_client.embed(
                    texts,
                    model=VOYAGE_MODEL,
                    input_type="document"
                )
                for text, embedding in zip(texts, result.embeddings):
                    results[text] = embedding
        
        except Exception as e:
            logger.error(f"Failed to generate batch embeddings: {e}")
        
        return results
    
    def get_embedding_dimension(self, embedding_type: str = "local") -> int:
        """Get the dimension of embeddings for a given type."""
        if "local" in embedding_type:
            return 384  # all-MiniLM-L6-v2 dimension
        else:
            return 1024  # voyage-3-lite dimension
    
    def clear_cache(self):
        """Clear the embedding cache."""
        self.embedding_cache.clear()
        logger.info("Cleared embedding cache")