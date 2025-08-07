#!/usr/bin/env python3
"""
Focused Verification Script for "cererbras" Content

Quick check to verify if the specific conversation chunks containing
the "cererbras" typo are properly indexed and searchable.
"""

import os
import sys
from pathlib import Path

# Add the project root to Python path for imports
project_root = Path(__file__).parent.parent
sys.path.insert(0, str(project_root / "mcp-server" / "src"))

from qdrant_client import QdrantClient

def check_cererbras_content():
    """Quick verification of cererbras content in both embedding modes"""
    client = QdrantClient(url="http://localhost:6333")
    
    # Expected collections for claude-self-reflect
    collections_to_check = ["conv_7f6df0fc_voyage", "conv_7f6df0fc_local"]
    
    # Get all collections
    collections = client.get_collections().collections
    collection_names = [c.name for c in collections]
    
    print("= CERERBRAS CONTENT VERIFICATION")
    print("=" * 50)
    
    found_collections = []
    for collection_name in collections_to_check:
        if collection_name in collection_names:
            found_collections.append(collection_name)
            
    if not found_collections:
        print("L No expected collections found!")
        print("Available collections:")
        for name in collection_names[:5]:
            print(f"   - {name}")
        return False
    
    print(f" Found collections: {found_collections}")
    
    # Test searches for each collection
    search_terms = ["cererbras", "openrouter", "claude code router"]
    results_found = False
    
    for collection_name in found_collections:
        print(f"\n=Ê Checking {collection_name}:")
        
        # Get collection info
        try:
            info = client.get_collection(collection_name)
            point_count = info.points_count
            print(f"   Points: {point_count}")
            
            if point_count == 0:
                print("      Collection is empty")
                continue
                
            # Search for content containing our terms
            # Use scroll to find any matching payload content
            scroll_result = client.scroll(
                collection_name=collection_name,
                limit=100,  # Check first 100 points
                with_payload=True
            )
            
            matches = []
            for point in scroll_result[0]:
                payload_text = str(point.payload).lower()
                for term in search_terms:
                    if term.lower() in payload_text:
                        matches.append({
                            "term": term,
                            "id": point.id,
                            "snippet": payload_text[:200] + "..." if len(payload_text) > 200 else payload_text
                        })
            
            if matches:
                results_found = True
                print(f"    Found {len(matches)} matches:")
                for match in matches[:3]:  # Show first 3 matches
                    print(f"      - '{match['term']}' in point {match['id']}")
                    print(f"        Snippet: {match['snippet'][:100]}...")
            else:
                print("   L No matching content found")
                
        except Exception as e:
            print(f"   L Error checking collection: {e}")
    
    print(f"\n<¯ RESULT: {' Content found' if results_found else 'L Content not found'}")
    
    if not results_found:
        print("\n=¡ NEXT STEPS:")
        print("   1. Check if these files are imported:")
        print("      - 6e38221d-df4c-4c19-a1be-e19472ecbb48.jsonl")
        print("      - d7f32965-9749-4fae-9b94-df83284537b6.jsonl")
        print("   2. Run import with these files specifically")
        print("   3. Check import logs for any errors")
    
    return results_found

if __name__ == "__main__":
    check_cererbras_content()