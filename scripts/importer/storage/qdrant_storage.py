"""Qdrant vector database storage implementation."""

import logging
from typing import List, Optional, Dict, Any
from qdrant_client import QdrantClient
from qdrant_client.models import (
    Distance,
    VectorParams,
    PointStruct,
    CollectionInfo
)

from ..core import ProcessedPoint
from ..core.exceptions import StorageError

logger = logging.getLogger(__name__)


class QdrantStorage:
    """
    Qdrant storage backend implementation.
    
    Handles all interactions with the Qdrant vector database.
    """
    
    def __init__(self, url: str = "http://localhost:6333", api_key: Optional[str] = None):
        self.url = url
        self.api_key = api_key
        self.client = None
        self._initialized = False
    
    def initialize(self) -> None:
        """Initialize connection to Qdrant."""
        try:
            self.client = QdrantClient(
                url=self.url,
                api_key=self.api_key,
                timeout=30
            )
            
            # Test connection
            self.client.get_collections()
            self._initialized = True
            logger.info(f"Connected to Qdrant at {self.url}")
            
        except Exception as e:
            raise StorageError(
                operation="initialize",
                collection="N/A",
                reason=f"Failed to connect to Qdrant: {e}"
            )
    
    def create_collection(self, name: str, dimension: int) -> bool:
        """
        Create a new collection if it doesn't exist.
        
        Args:
            name: Collection name
            dimension: Vector dimension
            
        Returns:
            True if created, False if already exists
        """
        if not self._initialized:
            raise StorageError(
                operation="create_collection",
                collection=name,
                reason="Storage not initialized"
            )
        
        try:
            if self.collection_exists(name):
                logger.debug(f"Collection {name} already exists")
                return False
            
            self.client.create_collection(
                collection_name=name,
                vectors_config=VectorParams(
                    size=dimension,
                    distance=Distance.COSINE
                )
            )
            
            logger.info(f"Created collection {name} with dimension {dimension}")
            return True
            
        except Exception as e:
            raise StorageError(
                operation="create_collection",
                collection=name,
                reason=str(e)
            )
    
    def collection_exists(self, name: str) -> bool:
        """Check if a collection exists."""
        if not self._initialized:
            raise StorageError(
                operation="collection_exists",
                collection=name,
                reason="Storage not initialized"
            )
        
        try:
            collections = self.client.get_collections().collections
            return any(c.name == name for c in collections)
        except Exception as e:
            logger.error(f"Failed to check collection existence: {e}")
            return False
    
    def upsert_points(self, collection: str, points: List[ProcessedPoint]) -> int:
        """
        Insert or update points in a collection.
        
        Args:
            collection: Collection name
            points: List of points to upsert
            
        Returns:
            Number of points upserted
        """
        if not self._initialized:
            raise StorageError(
                operation="upsert_points",
                collection=collection,
                reason="Storage not initialized"
            )
        
        if not points:
            return 0
        
        try:
            # Ensure collection exists with correct dimension
            if not self.collection_exists(collection):
                dimension = len(points[0].vector)
                self.create_collection(collection, dimension)
            
            # Convert to Qdrant points
            qdrant_points = [
                PointStruct(
                    id=self._generate_point_id(point.id),
                    vector=point.vector,
                    payload=point.payload
                )
                for point in points
            ]
            
            # Batch upsert
            operation_info = self.client.upsert(
                collection_name=collection,
                points=qdrant_points
            )
            
            logger.debug(f"Upserted {len(points)} points to {collection}")
            return len(points)
            
        except Exception as e:
            raise StorageError(
                operation="upsert_points",
                collection=collection,
                reason=str(e)
            )
    
    def get_collection_info(self, name: str) -> Optional[CollectionInfo]:
        """Get information about a collection."""
        if not self._initialized:
            return None
        
        try:
            return self.client.get_collection(collection_name=name)
        except Exception as e:
            logger.error(f"Failed to get collection info: {e}")
            return None
    
    def delete_collection(self, name: str) -> bool:
        """Delete a collection."""
        if not self._initialized:
            raise StorageError(
                operation="delete_collection",
                collection=name,
                reason="Storage not initialized"
            )
        
        try:
            if not self.collection_exists(name):
                return False
            
            self.client.delete_collection(collection_name=name)
            logger.info(f"Deleted collection {name}")
            return True
            
        except Exception as e:
            raise StorageError(
                operation="delete_collection",
                collection=name,
                reason=str(e)
            )
    
    def get_collections(self) -> List[str]:
        """Get list of all collection names."""
        if not self._initialized:
            return []
        
        try:
            collections = self.client.get_collections().collections
            return [c.name for c in collections]
        except Exception as e:
            logger.error(f"Failed to get collections: {e}")
            return []
    
    def count_points(self, collection: str) -> int:
        """Count points in a collection."""
        if not self._initialized:
            return 0
        
        try:
            info = self.get_collection_info(collection)
            return info.points_count if info else 0
        except Exception as e:
            logger.error(f"Failed to count points: {e}")
            return 0
    
    def _generate_point_id(self, string_id: str) -> str:
        """
        Generate a valid point ID from a string.
        
        Qdrant accepts string IDs directly in newer versions.
        For older versions, we might need to hash to integer.
        """
        # For now, use string IDs directly
        # If needed, can hash: int(hashlib.md5(string_id.encode()).hexdigest()[:16], 16)
        return string_id
    
    def health_check(self) -> Dict[str, Any]:
        """Check health of Qdrant connection."""
        if not self._initialized:
            return {"healthy": False, "reason": "Not initialized"}
        
        try:
            collections = self.client.get_collections()
            return {
                "healthy": True,
                "url": self.url,
                "collections_count": len(collections.collections),
                "total_points": sum(
                    self.count_points(c.name) 
                    for c in collections.collections
                )
            }
        except Exception as e:
            return {"healthy": False, "reason": str(e)}