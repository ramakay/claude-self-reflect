#!/usr/bin/env python3
"""
Pattern Propagator for Claude Self-Reflect
Propagates AST patterns from code-containing chunks to all chunks in a conversation
with distance-based weighting for semantic accuracy.
"""

import os
import logging
from typing import Dict, List, Optional, Set, Any
from datetime import datetime, timezone
from pathlib import Path
from qdrant_client import QdrantClient
from qdrant_client.models import Filter, FieldCondition, MatchValue
import time

# Setup logging
logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

class PatternPropagator:
    """Propagates patterns across conversation chunks with weighted confidence"""
    
    def __init__(self, client: Optional[QdrantClient] = None):
        """Initialize with Qdrant client"""
        if client:
            self.client = client
        else:
            self.client = QdrantClient(
                url=os.getenv("QDRANT_URL", "http://localhost:6333"),
                api_key=os.getenv("QDRANT_API_KEY")
            )
    
    def get_conversation_chunks(self, collection_name: str, conversation_id: str) -> List[Dict]:
        """Get all chunks for a conversation"""
        chunks = []
        offset = None
        
        while True:
            result = self.client.scroll(
                collection_name=collection_name,
                scroll_filter=Filter(
                    must=[
                        FieldCondition(
                            key="conversation_id",
                            match=MatchValue(value=conversation_id)
                        )
                    ]
                ),
                limit=100,
                offset=offset,
                with_payload=True,
                with_vectors=False
            )
            
            points, next_offset = result
            if not points:
                break
                
            for point in points:
                chunks.append({
                    "id": point.id,
                    "chunk_index": point.payload.get("chunk_index", 0),
                    "patterns": point.payload.get("code_patterns"),
                    "payload": point.payload
                })
            
            offset = next_offset
            if offset is None:
                break
        
        # Sort by chunk index
        chunks.sort(key=lambda x: x["chunk_index"])
        return chunks
    
    def calculate_confidence(self, distance: int) -> float:
        """Calculate confidence score based on distance from source chunk"""
        # Confidence decays with distance
        # Adjacent chunks: 0.9, 2 chunks away: 0.8, etc.
        # Minimum confidence: 0.3
        return max(0.3, 1.0 - (distance * 0.1))
    
    def aggregate_patterns(self, source_chunks: List[Dict]) -> Dict:
        """Aggregate patterns from all source chunks"""
        aggregated = {}
        
        for chunk in source_chunks:
            if chunk["patterns"]:
                for category, patterns in chunk["patterns"].items():
                    if category not in aggregated:
                        aggregated[category] = set()
                    aggregated[category].update(patterns)
        
        # Convert sets back to lists
        return {cat: list(pats) for cat, pats in aggregated.items()}
    
    def propagate_patterns_weighted(self, collection_name: str, conversation_id: str) -> Dict:
        """
        Propagate patterns with distance-based weighting
        Returns statistics about the propagation
        """
        stats = {
            "conversation_id": conversation_id,
            "total_chunks": 0,
            "source_chunks": 0,
            "propagated_to": 0,
            "patterns_found": {},
            "errors": []
        }
        
        try:
            # Get all chunks for conversation
            chunks = self.get_conversation_chunks(collection_name, conversation_id)
            stats["total_chunks"] = len(chunks)
            
            if not chunks:
                logger.warning(f"No chunks found for conversation {conversation_id}")
                return stats
            
            # Identify chunks with original patterns
            source_chunks = [c for c in chunks if c["patterns"]]
            stats["source_chunks"] = len(source_chunks)
            
            if not source_chunks:
                logger.info(f"No patterns to propagate for conversation {conversation_id}")
                return stats
            
            # Aggregate all patterns from source chunks
            aggregated_patterns = self.aggregate_patterns(source_chunks)
            stats["patterns_found"] = {cat: len(pats) for cat, pats in aggregated_patterns.items()}
            
            # Build index of source chunk positions
            source_positions = {c["chunk_index"]: c for c in source_chunks}
            
            # Propagate to chunks without patterns
            for chunk in chunks:
                if chunk["patterns"]:
                    # Skip chunks that already have patterns
                    continue
                
                # Find nearest source chunk
                chunk_position = chunk["chunk_index"]
                distances = [abs(pos - chunk_position) for pos in source_positions.keys()]
                
                if not distances:
                    continue
                    
                min_distance = min(distances)
                nearest_idx = distances.index(min_distance)
                nearest_source_pos = list(source_positions.keys())[nearest_idx]
                nearest_source = source_positions[nearest_source_pos]
                
                # Calculate confidence based on distance
                confidence = self.calculate_confidence(min_distance)
                
                # Prepare inherited patterns with metadata
                inherited_payload = {
                    "code_patterns": aggregated_patterns,  # Use aggregated patterns
                    "pattern_inheritance": {
                        "inherited": True,
                        "source_chunk_ids": [c["id"] for c in source_chunks],
                        "nearest_source_id": str(nearest_source["id"]),
                        "distance": min_distance,
                        "confidence": confidence,
                        "propagated_at": datetime.now(timezone.utc).isoformat(),
                        "aggregated": True
                    }
                }
                
                # Update the chunk in Qdrant
                try:
                    self.client.set_payload(
                        collection_name=collection_name,
                        payload=inherited_payload,
                        points=[chunk["id"]],
                        wait=False
                    )
                    stats["propagated_to"] += 1
                    logger.debug(f"Propagated patterns to chunk {chunk['id']} with confidence {confidence:.2f}")
                except Exception as e:
                    error_msg = f"Failed to update chunk {chunk['id']}: {e}"
                    logger.error(error_msg)
                    stats["errors"].append(error_msg)
            
            logger.info(f"Propagated patterns to {stats['propagated_to']} chunks in conversation {conversation_id}")
            
        except Exception as e:
            error_msg = f"Error processing conversation {conversation_id}: {e}"
            logger.error(error_msg)
            stats["errors"].append(error_msg)
        
        return stats
    
    def propagate_collection(self, collection_name: str, limit: Optional[int] = None) -> Dict:
        """
        Propagate patterns for all conversations in a collection
        """
        overall_stats = {
            "collection": collection_name,
            "conversations_processed": 0,
            "total_propagations": 0,
            "start_time": datetime.now(timezone.utc).isoformat(),
            "errors": []
        }
        
        try:
            # Get unique conversation IDs
            conversation_ids = set()
            offset = None
            
            while True:
                result = self.client.scroll(
                    collection_name=collection_name,
                    limit=100,
                    offset=offset,
                    with_payload=["conversation_id"],
                    with_vectors=False
                )
                
                points, next_offset = result
                if not points:
                    break
                
                for point in points:
                    conv_id = point.payload.get("conversation_id")
                    if conv_id:
                        conversation_ids.add(conv_id)
                
                offset = next_offset
                if offset is None:
                    break
            
            logger.info(f"Found {len(conversation_ids)} unique conversations in {collection_name}")
            
            # Process each conversation
            for i, conv_id in enumerate(conversation_ids):
                if limit and i >= limit:
                    break
                
                logger.info(f"Processing conversation {i+1}/{len(conversation_ids)}: {conv_id}")
                stats = self.propagate_patterns_weighted(collection_name, conv_id)
                
                overall_stats["conversations_processed"] += 1
                overall_stats["total_propagations"] += stats["propagated_to"]
                overall_stats["errors"].extend(stats["errors"])
            
            overall_stats["end_time"] = datetime.now(timezone.utc).isoformat()
            
        except Exception as e:
            error_msg = f"Error processing collection {collection_name}: {e}"
            logger.error(error_msg)
            overall_stats["errors"].append(error_msg)
        
        return overall_stats


def main():
    """Main execution function"""
    import argparse
    
    parser = argparse.ArgumentParser(description="Propagate AST patterns across conversation chunks")
    parser.add_argument("--collection", help="Collection name to process (or 'all' for all collections)")
    parser.add_argument("--conversation", help="Specific conversation ID to process")
    parser.add_argument("--limit", type=int, help="Limit number of conversations/collections to process")
    parser.add_argument("--project", help="Filter by project name")
    
    args = parser.parse_args()
    
    propagator = PatternPropagator()
    
    if args.conversation and args.collection:
        # Process single conversation
        stats = propagator.propagate_patterns_weighted(args.collection, args.conversation)
        print(f"\nPropagation complete:")
        print(f"  Total chunks: {stats['total_chunks']}")
        print(f"  Source chunks: {stats['source_chunks']}")
        print(f"  Propagated to: {stats['propagated_to']}")
        print(f"  Patterns found: {stats['patterns_found']}")
        if stats['errors']:
            print(f"  Errors: {len(stats['errors'])}")
    elif args.collection == 'all' or not args.collection:
        # Process all collections
        collections = propagator.client.get_collections().collections
        overall_stats = {
            "collections_processed": 0,
            "total_propagations": 0,
            "errors": []
        }
        
        collection_limit = args.limit if args.limit else len(collections)
        
        for i, collection in enumerate(collections[:collection_limit]):
            print(f"\nProcessing collection {i+1}/{min(collection_limit, len(collections))}: {collection.name}")
            stats = propagator.propagate_collection(collection.name, limit=None)
            overall_stats["collections_processed"] += stats["conversations_processed"]
            overall_stats["total_propagations"] += stats["total_propagations"]
            overall_stats["errors"].extend(stats["errors"])
        
        print(f"\n\nOverall propagation complete:")
        print(f"  Collections processed: {min(collection_limit, len(collections))}")
        print(f"  Conversations processed: {overall_stats['collections_processed']}")
        print(f"  Total propagations: {overall_stats['total_propagations']}")
        if overall_stats['errors']:
            print(f"  Total errors: {len(overall_stats['errors'])}")
    else:
        # Process entire collection
        stats = propagator.propagate_collection(args.collection, limit=args.limit)
        print(f"\nCollection propagation complete:")
        print(f"  Conversations processed: {stats['conversations_processed']}")
        print(f"  Total propagations: {stats['total_propagations']}")
        if stats['errors']:
            print(f"  Errors: {len(stats['errors'])}")


if __name__ == "__main__":
    main()