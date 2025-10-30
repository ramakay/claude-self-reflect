"""
Qdrant connection utilities with retry logic.
"""

import time
import logging
from typing import Optional
from qdrant_client import QdrantClient

logger = logging.getLogger(__name__)


def connect_to_qdrant_with_retry(
    url: str,
    api_key: Optional[str] = None,
    max_retries: int = 5,
    initial_delay: float = 1.0
) -> QdrantClient:
    """
    Connect to Qdrant with exponential backoff retry logic.

    Args:
        url: Qdrant URL
        api_key: Optional API key for authentication
        max_retries: Maximum number of retry attempts (default: 5)
        initial_delay: Initial delay in seconds, doubles each retry (default: 1.0)

    Returns:
        Connected QdrantClient instance

    Raises:
        Exception: If all retries fail

    Example:
        >>> client = connect_to_qdrant_with_retry(
        ...     url="http://localhost:6333",
        ...     api_key="optional-api-key"
        ... )
        ✅ Connected to Qdrant at http://localhost:6333
    """
    delay = initial_delay

    for attempt in range(max_retries):
        try:
            # Initialize client
            if api_key:
                client = QdrantClient(url=url, api_key=api_key)
            else:
                client = QdrantClient(url=url)

            # Test connection by fetching collections
            client.get_collections()

        except Exception as e:
            if attempt < max_retries - 1:
                logger.warning(
                    f"⚠️  Qdrant connection attempt {attempt + 1}/{max_retries} failed: {e}"
                )
                logger.info(f"   Retrying in {delay}s...")
                time.sleep(delay)
                delay *= 2  # Exponential backoff
            else:
                logger.exception(
                    f"Failed to connect to Qdrant after {max_retries} attempts"
                )
                raise
        else:
            # Connection successful
            logger.info(f"✅ Connected to Qdrant at {url}")
            return client

    # Should never reach here due to raise in except block
    raise Exception(f"Failed to connect to Qdrant at {url} after {max_retries} attempts")
