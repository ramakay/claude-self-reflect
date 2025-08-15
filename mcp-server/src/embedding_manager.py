"""Robust embedding model manager with proper cache handling."""

import os
import sys
import time
import logging
import shutil
from typing import Optional, List, Union
from pathlib import Path
import threading
import signal

logger = logging.getLogger(__name__)

class EmbeddingManager:
    """Manages embedding models with proper cache and lock handling."""
    
    def __init__(self):
        self.model = None
        self.model_type = None  # 'local' or 'voyage'
        self.voyage_client = None
        
        # Configuration
        self.prefer_local = os.getenv('PREFER_LOCAL_EMBEDDINGS', 'true').lower() == 'true'
        self.voyage_key = os.getenv('VOYAGE_KEY') or os.getenv('VOYAGE_KEY-2')
        self.embedding_model = os.getenv('EMBEDDING_MODEL', 'sentence-transformers/all-MiniLM-L6-v2')
        self.download_timeout = int(os.getenv('FASTEMBED_DOWNLOAD_TIMEOUT', '30'))
        
        # Set cache directory to our controlled location
        self.cache_dir = Path(__file__).parent.parent / '.fastembed-cache'
        
    def _clean_stale_locks(self):
        """Clean up any stale lock files from previous runs."""
        locks_dir = self.cache_dir / '.locks'
        if locks_dir.exists():
            logger.info(f"Cleaning stale locks in {locks_dir}")
            try:
                # Remove all lock files older than 5 minutes
                import time
                current_time = time.time()
                for lock_file in locks_dir.glob('**/*.lock'):
                    try:
                        age = current_time - lock_file.stat().st_mtime
                        if age > 300:  # 5 minutes
                            lock_file.unlink()
                            logger.debug(f"Removed stale lock: {lock_file.name}")
                    except Exception as e:
                        logger.debug(f"Could not remove lock {lock_file}: {e}")
            except Exception as e:
                logger.warning(f"Error cleaning locks: {e}")
        
    def initialize(self) -> bool:
        """Initialize embedding model based on user preference."""
        logger.info("Initializing embedding manager...")
        
        # Clean up any stale locks first
        self._clean_stale_locks()
        
        if self.prefer_local:
            # User wants local - try local only, don't fallback to cloud
            if self._try_initialize_local():
                return True
            logger.error("Local embeddings failed and user prefers local - not falling back to cloud")
            return False
        else:
            # User prefers Voyage AI
            if self.voyage_key and self._try_initialize_voyage():
                return True
            logger.warning("Voyage AI failed, trying local as fallback...")
            if self._try_initialize_local():
                return True
            logger.error("Both Voyage AI and local embeddings failed")
            return False
    
    def _try_initialize_local(self) -> bool:
        """Try to initialize local FastEmbed model with timeout and optimizations."""
        try:
            logger.info(f"Attempting to load local model: {self.embedding_model}")
            
            # CRITICAL OPTIMIZATION: Set thread limits BEFORE loading model
            # This prevents ONNX Runtime and BLAS from over-subscribing CPU
            os.environ['OMP_NUM_THREADS'] = '1'
            os.environ['MKL_NUM_THREADS'] = '1' 
            os.environ['OPENBLAS_NUM_THREADS'] = '1'
            os.environ['NUMEXPR_NUM_THREADS'] = '1'
            logger.info("Set thread limits to prevent CPU over-subscription")
            
            # Ensure cache directory exists and is writable
            self.cache_dir.mkdir(parents=True, exist_ok=True)
            
            # Set FASTEMBED_CACHE_PATH to our controlled directory
            os.environ['FASTEMBED_CACHE_PATH'] = str(self.cache_dir)
            logger.info(f"Using cache directory: {self.cache_dir}")
            
            # Also set HF_HOME to avoid any HuggingFace cache issues
            os.environ['HF_HOME'] = str(self.cache_dir / 'huggingface')
            
            model_cache = self.cache_dir / 'models--qdrant--all-MiniLM-L6-v2-onnx'
            
            if model_cache.exists():
                logger.info("Model cache found, loading from cache...")
            else:
                logger.info(f"Model cache not found, will download (timeout: {self.download_timeout}s)")
                logger.info("Note: First download may take 1-2 minutes")
                
            # Force alternative download if HuggingFace is problematic
            # This uses Qdrant's CDN which is more reliable
            if os.getenv('FASTEMBED_SKIP_HUGGINGFACE', 'true').lower() == 'true':
                os.environ['HF_HUB_OFFLINE'] = '1'
                logger.info("Using alternative download sources (Qdrant CDN)")
            
            # Use a thread with timeout for model initialization
            success = False
            error = None
            
            def init_model():
                nonlocal success, error
                try:
                    from fastembed import TextEmbedding
                    # Initialize with optimized settings
                    # Note: FastEmbed uses these environment variables internally
                    self.model = TextEmbedding(
                        model_name=self.embedding_model,
                        threads=1  # Single thread per worker to prevent over-subscription
                    )
                    self.model_type = 'local'
                    success = True
                    logger.info(f"Successfully initialized local model: {self.embedding_model} with single-thread mode")
                except Exception as e:
                    error = e
                    logger.error(f"Failed to initialize local model: {e}")
            
            # Start initialization in a thread
            thread = threading.Thread(target=init_model)
            thread.daemon = True
            thread.start()
            thread.join(timeout=self.download_timeout)
            
            if thread.is_alive():
                logger.error(f"Model initialization timed out after {self.download_timeout}s")
                logger.info("Tip: Set FASTEMBED_SKIP_HUGGINGFACE=true to use alternative download sources")
                # Thread will continue in background but we move on
                return False
            
            return success
            
        except ImportError:
            logger.error("FastEmbed not installed. Install with: pip install fastembed")
            return False
        except Exception as e:
            logger.error(f"Unexpected error initializing local embeddings: {e}")
            return False
    
    def _try_initialize_voyage(self) -> bool:
        """Try to initialize Voyage AI client."""
        try:
            logger.info("Attempting to initialize Voyage AI...")
            import voyageai
            self.voyage_client = voyageai.Client(api_key=self.voyage_key)
            
            # Test the client with a simple embedding
            test_result = self.voyage_client.embed(
                texts=["test"],
                model="voyage-3",
                input_type="document"
            )
            
            if test_result and test_result.embeddings:
                self.model_type = 'voyage'
                logger.info("Successfully initialized Voyage AI")
                return True
            else:
                logger.error("Voyage AI test embedding failed")
                return False
                
        except Exception as e:
            logger.error(f"Failed to initialize Voyage AI: {e}")
            return False
    
    def embed(self, texts: Union[str, List[str]], input_type: str = "document") -> Optional[List[List[float]]]:
        """Generate embeddings using the active model."""
        if not self.model and not self.voyage_client:
            logger.error("No embedding model initialized")
            return None
        
        # Ensure texts is a list
        if isinstance(texts, str):
            texts = [texts]
        
        try:
            if self.model_type == 'local':
                # FastEmbed returns a generator, convert to list
                embeddings = list(self.model.embed(texts))
                return [emb.tolist() for emb in embeddings]
            
            elif self.model_type == 'voyage':
                result = self.voyage_client.embed(
                    texts=texts,
                    model="voyage-3-lite" if input_type == "query" else "voyage-3",
                    input_type=input_type
                )
                return result.embeddings
            
        except Exception as e:
            logger.error(f"Error generating embeddings: {e}")
            return None
    
    def get_vector_dimension(self) -> int:
        """Get the dimension of embeddings."""
        if self.model_type == 'local':
            return 384  # all-MiniLM-L6-v2 dimension
        elif self.model_type == 'voyage':
            return 1024  # voyage-3 dimension
        return 0
    
    def get_model_info(self) -> dict:
        """Get information about the active model."""
        return {
            'type': self.model_type,
            'model': self.embedding_model if self.model_type == 'local' else 'voyage-3',
            'dimension': self.get_vector_dimension(),
            'prefer_local': self.prefer_local,
            'has_voyage_key': bool(self.voyage_key)
        }


# Global instance
_embedding_manager = None

def get_embedding_manager() -> EmbeddingManager:
    """Get or create the global embedding manager."""
    global _embedding_manager
    if _embedding_manager is None:
        _embedding_manager = EmbeddingManager()
        if not _embedding_manager.initialize():
            raise RuntimeError("Failed to initialize any embedding model")
    return _embedding_manager