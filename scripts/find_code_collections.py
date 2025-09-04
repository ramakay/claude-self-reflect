#!/usr/bin/env python3
"""Find collections containing code"""

from qdrant_client import QdrantClient
import os

client = QdrantClient(
    url=os.getenv("QDRANT_URL", "http://localhost:6333"),
    api_key=os.getenv("QDRANT_API_KEY")
)

# Search for collections with code content
collections = client.get_collections().collections

found_code = []
for collection in collections[:50]:
    try:
        # Sample some points
        result = client.scroll(
            collection_name=collection.name,
            limit=10,
            with_payload=['text'],
            with_vectors=False
        )
        
        points, _ = result
        for point in points:
            text = point.payload.get('text', '')
            # Check for code markers
            if '```' in text and len(text) > 500:
                # Count code blocks
                code_blocks = text.count('```')
                if code_blocks >= 2:
                    found_code.append({
                        'collection': collection.name,
                        'point_id': str(point.id),
                        'code_blocks': code_blocks // 2,
                        'text_length': len(text),
                        'sample': text[:200]
                    })
                    break
        
        if len(found_code) >= 10:
            break
            
    except Exception as e:
        pass

print(f'Found {len(found_code)} collections with code content:\n')
for item in found_code[:5]:
    print(f"Collection: {item['collection']}")
    print(f"  Code blocks: {item['code_blocks']}")
    print(f"  Text length: {item['text_length']}")
    print(f"  Sample: {item['sample'][:100]}...")
    print()