#!/usr/bin/env python3
"""
Verify that patterns are stored in Qdrant and can be retrieved
"""

import os
import logging
from qdrant_client import QdrantClient
from qdrant_client.models import Filter, FieldCondition, MatchValue

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

def verify_patterns():
    """Check patterns in collections"""
    client = QdrantClient(
        url=os.getenv("QDRANT_URL", "http://localhost:6333"),
        api_key=os.getenv("QDRANT_API_KEY")
    )
    
    # Get all collections
    collections = client.get_collections().collections
    
    total_points_with_patterns = 0
    total_points_with_inheritance = 0
    collections_with_patterns = []
    
    for collection in collections[:20]:  # Check first 20 collections
        try:
            # Check for any patterns
            result = client.scroll(
                collection_name=collection.name,
                scroll_filter=Filter(
                    must=[
                        FieldCondition(
                            key="code_patterns",
                            match=MatchValue(value={"$exists": True})
                        )
                    ]
                ),
                limit=1,
                with_payload=False,
                with_vectors=False
            )
            
            if result[0]:  # Has points with patterns
                # Count total points with patterns
                count_result = client.count(
                    collection_name=collection.name,
                    count_filter=Filter(
                        must=[
                            FieldCondition(
                                key="pattern_inheritance.inherited",
                                match=MatchValue(value=True)
                            )
                        ]
                    )
                )
                
                inherited_count = count_result.count
                
                # Get a sample point with patterns
                sample_result = client.scroll(
                    collection_name=collection.name,
                    scroll_filter=Filter(
                        must=[
                            FieldCondition(
                                key="pattern_inheritance.inherited",
                                match=MatchValue(value=True)
                            )
                        ]
                    ),
                    limit=1,
                    with_payload=True,
                    with_vectors=False
                )
                
                if sample_result[0]:
                    point = sample_result[0][0]
                    patterns = point.payload.get('code_patterns', {})
                    collections_with_patterns.append({
                        'name': collection.name,
                        'inherited_count': inherited_count,
                        'sample_patterns': list(patterns.keys()) if patterns else []
                    })
                    total_points_with_inheritance += inherited_count
                    
        except Exception as e:
            logger.debug(f"Error checking {collection.name}: {e}")
    
    print(f"\n=== PATTERN VERIFICATION RESULTS ===")
    print(f"Collections checked: 20")
    print(f"Collections with inherited patterns: {len(collections_with_patterns)}")
    print(f"Total points with inherited patterns: {total_points_with_inheritance}")
    
    if collections_with_patterns:
        print(f"\nTop collections with patterns:")
        for col in collections_with_patterns[:5]:
            print(f"  - {col['name']}: {col['inherited_count']} inherited points")
            if col['sample_patterns']:
                print(f"    Sample categories: {', '.join(col['sample_patterns'])}")
    
    return collections_with_patterns

if __name__ == "__main__":
    verify_patterns()