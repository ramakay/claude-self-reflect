#!/usr/bin/env python3
"""
CSR Standalone Client - For use outside the MCP server context.

This client provides search and store_reflection functionality
for hooks and scripts that need to interact with CSR without
going through the MCP protocol.

Usage:
    from mcp_server.src.standalone_client import CSRStandaloneClient

    client = CSRStandaloneClient()
    results = client.search("docker issues", limit=5)
    client.store_reflection("Key insight here", tags=["insight"])
"""

import os
import uuid
import hashlib
import logging
from typing import List, Dict, Any, Optional
from datetime import datetime, timezone
from pathlib import Path

logger = logging.getLogger(__name__)


class CSRStandaloneClient:
    """Standalone CSR client for hooks and scripts."""

    def __init__(
        self,
        qdrant_url: str = None,
        qdrant_api_key: str = None,
        prefer_local: bool = None
    ):
        """Initialize the standalone client.

        Args:
            qdrant_url: Qdrant server URL (default: from env or localhost:6333)
            qdrant_api_key: Qdrant API key (default: from env)
            prefer_local: Use local embeddings (default: from env or True)
        """
        self.qdrant_url = qdrant_url or os.getenv('QDRANT_URL', 'http://localhost:6333')
        self.qdrant_api_key = qdrant_api_key or os.getenv('QDRANT_API_KEY')

        if prefer_local is None:
            self.prefer_local = os.getenv('PREFER_LOCAL_EMBEDDINGS', 'true').lower() == 'true'
        else:
            self.prefer_local = prefer_local

        self._client = None
        self._embedding_manager = None

    def _get_client(self):
        """Get or create Qdrant client (synchronous)."""
        if self._client is None:
            from qdrant_client import QdrantClient

            # Parse URL for host/port
            import urllib.parse
            parsed = urllib.parse.urlparse(self.qdrant_url)
            host = parsed.hostname or 'localhost'
            port = parsed.port or 6333

            self._client = QdrantClient(
                host=host,
                port=port,
                api_key=self.qdrant_api_key if self.qdrant_api_key else None,
                timeout=30
            )
        return self._client

    def _get_embedding_manager(self):
        """Get or create embedding manager."""
        if self._embedding_manager is None:
            # Try to use the project's embedding manager
            try:
                from .embedding_manager import EmbeddingManager
                self._embedding_manager = EmbeddingManager()
            except ImportError:
                # Fallback: create minimal embedding functionality
                self._embedding_manager = self._create_fallback_embeddings()
        return self._embedding_manager

    def _create_fallback_embeddings(self):
        """Create fallback embedding functionality using fastembed."""
        class FallbackEmbeddings:
            def __init__(self):
                self._model = None

            def get_model(self):
                if self._model is None:
                    try:
                        from fastembed import TextEmbedding
                        self._model = TextEmbedding("BAAI/bge-small-en-v1.5")
                    except ImportError:
                        raise ImportError("fastembed not installed. Run: pip install fastembed")
                return self._model

            def embed(self, text: str) -> List[float]:
                model = self.get_model()
                embeddings = list(model.embed([text]))
                return list(embeddings[0])

            @property
            def dimension(self) -> int:
                return 384

        return FallbackEmbeddings()

    def search(
        self,
        query: str,
        limit: int = 5,
        min_score: float = 0.3,
        project: str = None
    ) -> List[Dict[str, Any]]:
        """Search for relevant conversations.

        Args:
            query: Search query
            limit: Maximum results to return
            min_score: Minimum similarity score
            project: Project name filter (optional)

        Returns:
            List of search results with content and metadata
        """
        client = self._get_client()
        embeddings = self._get_embedding_manager()

        # Generate query embedding
        query_vector = embeddings.embed(query)

        # Find searchable collections
        collections = client.get_collections().collections
        searchable = [
            c.name for c in collections
            if self._is_searchable_collection(c.name)
        ]

        if not searchable:
            logger.warning("No searchable collections found")
            return []

        # Filter by project if specified
        if project and project != 'all':
            project_norm = self._normalize_project_name(project)
            searchable = [c for c in searchable if project_norm in c]

        # Prioritize reflections collections (where Ralph state is stored)
        reflections = [c for c in searchable if c.startswith('reflections')]
        others = [c for c in searchable if not c.startswith('reflections')]
        searchable = reflections + others

        results = []
        for collection_name in searchable[:8]:  # Search up to 8 collections
            try:
                search_results = client.search(
                    collection_name=collection_name,
                    query_vector=query_vector,
                    limit=limit,
                    score_threshold=min_score
                )

                for hit in search_results:
                    payload = hit.payload or {}
                    results.append({
                        'score': hit.score,
                        'content': payload.get('content', ''),
                        'preview': payload.get('preview', payload.get('content', '')[:200]),
                        'metadata': {
                            'collection': collection_name,
                            'conversation_id': payload.get('conversation_id', ''),
                            'timestamp': payload.get('timestamp', ''),
                            'project': payload.get('project', ''),
                        }
                    })
            except Exception as e:
                logger.debug(f"Error searching {collection_name}: {e}")
                continue

        # Sort by score and limit
        results.sort(key=lambda x: x['score'], reverse=True)
        return results[:limit]

    def store_reflection(
        self,
        content: str,
        tags: List[str] = None
    ) -> str:
        """Store a reflection/insight.

        Args:
            content: The reflection content
            tags: Optional tags for categorization

        Returns:
            ID of stored reflection
        """
        tags = tags or []
        client = self._get_client()
        embeddings = self._get_embedding_manager()

        # Determine collection name
        collection_name = f"reflections_{'local' if self.prefer_local else 'voyage'}"

        # Ensure collection exists
        try:
            client.get_collection(collection_name)
        except Exception:
            # Create collection
            from qdrant_client.models import VectorParams, Distance
            client.create_collection(
                collection_name=collection_name,
                vectors_config=VectorParams(
                    size=embeddings.dimension,
                    distance=Distance.COSINE
                )
            )

        # Generate embedding
        vector = embeddings.embed(content)

        # Generate ID
        reflection_id = hashlib.sha256(
            f"{content}{datetime.now().isoformat()}".encode()
        ).hexdigest()[:16]

        # Store
        from qdrant_client.models import PointStruct
        client.upsert(
            collection_name=collection_name,
            points=[
                PointStruct(
                    id=str(uuid.uuid4()),
                    vector=vector,
                    payload={
                        "content": content,
                        "tags": tags,
                        "timestamp": datetime.now(timezone.utc).isoformat(),
                        "reflection_id": reflection_id,
                        "type": "reflection"
                    }
                )
            ]
        )

        logger.info(f"Stored reflection: {reflection_id}")
        return reflection_id

    def _is_searchable_collection(self, name: str) -> bool:
        """Check if collection is searchable."""
        return (
            name.endswith('_local')
            or name.endswith('_voyage')
            or name.endswith('_384d')
            or name.endswith('_1024d')
            or '_cloud_' in name
            or name.startswith('reflections')
            or name.startswith('csr_')
        )

    def _normalize_project_name(self, name: str) -> str:
        """Normalize project name for collection matching."""
        import re
        # Convert to lowercase, replace special chars
        normalized = name.lower()
        normalized = re.sub(r'[^a-z0-9]', '_', normalized)
        normalized = re.sub(r'_+', '_', normalized)
        return normalized.strip('_')

    def test_connection(self) -> bool:
        """Test if CSR is accessible.

        Returns:
            True if connection successful
        """
        try:
            client = self._get_client()
            client.get_collections()
            return True
        except Exception as e:
            logger.error(f"Connection test failed: {e}")
            return False


# Convenience function for quick searches
def quick_search(query: str, limit: int = 3) -> List[Dict[str, Any]]:
    """Quick search without creating client instance."""
    client = CSRStandaloneClient()
    return client.search(query, limit=limit)


if __name__ == "__main__":
    # Test the client
    import sys

    client = CSRStandaloneClient()

    if client.test_connection():
        print("✓ CSR connection successful")

        if len(sys.argv) > 1:
            query = " ".join(sys.argv[1:])
            results = client.search(query, limit=3)
            print(f"\nResults for '{query}':")
            for i, r in enumerate(results, 1):
                print(f"\n{i}. Score: {r['score']:.2f}")
                print(f"   {r['preview'][:100]}...")
    else:
        print("✗ CSR connection failed")
        sys.exit(1)
