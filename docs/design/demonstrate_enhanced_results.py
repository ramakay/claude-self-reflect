#!/usr/bin/env python3
"""
Demonstrate superior metadata-enriched narratives from V3+SKILL_V2+Metadata.

Shows actual search results with rich context vs old chunking system.
"""

import os
import sys
from pathlib import Path
from qdrant_client import QdrantClient
import json

# Setup
QDRANT_URL = os.getenv("QDRANT_URL", "http://localhost:6333")
client = QdrantClient(url=QDRANT_URL)

# Try importing FastEmbed
try:
    from fastembed import TextEmbedding
    embedding_model = TextEmbedding(model_name="sentence-transformers/all-MiniLM-L6-v2")
    print("Using FastEmbed for search")
except ImportError:
    print("❌ FastEmbed not available")
    sys.exit(1)


def get_embedding(text: str) -> list:
    """Generate embedding for search query."""
    embeddings = list(embedding_model.embed([text]))
    return embeddings[0].tolist()


def demonstrate_enhanced_search(query: str, collection: str = "v3_all_projects", limit: int = 3):
    """
    Run a search and display the metadata-enriched results.

    This shows the FULL narrative with problem-solution patterns,
    metadata context, and signature enrichment.
    """

    print(f"\n{'='*100}")
    print(f"SEARCH QUERY: {query}")
    print(f"{'='*100}\n")

    # Generate query embedding
    query_embedding = get_embedding(query)

    # Search Qdrant
    try:
        results = client.search(
            collection_name=collection,
            query_vector=query_embedding,
            limit=limit,
            with_payload=True
        )
    except Exception as e:
        print(f"❌ Search failed: {e}")
        return

    if not results:
        print("No results found.")
        return

    print(f"Found {len(results)} results:\n")

    for i, result in enumerate(results, 1):
        print(f"\n{'─'*100}")
        print(f"RESULT {i} | Score: {result.score:.4f}")
        print(f"{'─'*100}")

        payload = result.payload

        # Show conversation ID and project
        print(f"\n📌 Conversation: {payload.get('conversation_id', 'N/A')}")
        print(f"📁 Project: {payload.get('project', 'N/A')}")

        # Show metadata-enriched signature
        signature = payload.get('signature', {})
        print(f"\n📊 **METADATA-ENRICHED SIGNATURE:**")
        print(f"   Status: {signature.get('completion_status', 'N/A')}")
        print(f"   Frameworks: {', '.join(signature.get('frameworks', []))}")
        print(f"   Tools Used: {', '.join(signature.get('tools_used', []))[:80]}")
        print(f"   Concepts: {', '.join(signature.get('concepts', []))}")
        print(f"   Files Modified: {len(signature.get('files_modified', []))} files")
        print(f"   Edits: {signature.get('total_edits', 0)}")
        print(f"   Analysis Only: {signature.get('analysis_only', False)}")

        # Show the FULL narrative (not truncated)
        narrative = payload.get('narrative', 'No narrative available')
        print(f"\n📝 **ENHANCED NARRATIVE:**")
        print(f"{'-'*100}")
        print(narrative)
        print(f"{'-'*100}")

        # Show search index (what was embedded)
        search_index = payload.get('search_index', '')
        if search_index:
            print(f"\n🔍 **SEARCH INDEX (Embedded):**")
            print(search_index[:500] + "..." if len(search_index) > 500 else search_index)

    print(f"\n{'='*100}\n")


def compare_collection_stats():
    """Show collection statistics to demonstrate metadata enrichment."""

    collection_name = "v3_all_projects"

    try:
        collection_info = client.get_collection(collection_name)
        print(f"\n📦 **COLLECTION STATS: {collection_name}**")
        print(f"   Total points: {collection_info.points_count}")
        print(f"   Vector dimensions: {collection_info.config.params.vectors.size}")
        print(f"   Distance metric: {collection_info.config.params.vectors.distance}")

        # Sample a point to show metadata richness
        sample_results = client.scroll(
            collection_name=collection_name,
            limit=1,
            with_payload=True,
            with_vectors=False
        )

        if sample_results[0]:
            sample_payload = sample_results[0][0].payload
            signature = sample_payload.get('signature', {})

            print(f"\n📌 **SAMPLE POINT METADATA:**")
            print(f"   Has narrative: {'narrative' in sample_payload}")
            print(f"   Has search_index: {'search_index' in sample_payload}")
            print(f"   Has context_cache: {'context_cache' in sample_payload}")
            print(f"   Has signature: {'signature' in sample_payload}")
            print(f"   Signature keys: {list(signature.keys())}")

            # Show metadata enrichment
            if 'tools_used' in signature:
                print(f"\n✅ METADATA ENRICHMENT CONFIRMED:")
                print(f"   - Tools detected: {signature.get('tools_used', [])}")
                print(f"   - Concepts detected: {signature.get('concepts', [])}")
                print(f"   - Files modified: {len(signature.get('files_modified', []))}")
                print(f"   - Analysis only: {signature.get('analysis_only', False)}")
            else:
                print(f"\n⚠️  No metadata enrichment detected in signature")

        print()

    except Exception as e:
        print(f"❌ Error fetching collection stats: {e}")


def main():
    """Run demonstrations."""

    print(f"\n{'='*100}")
    print(f"V3+SKILL_V2+METADATA ENHANCED RESULTS DEMONSTRATION")
    print(f"{'='*100}")

    # Show collection stats
    compare_collection_stats()

    # Demo searches showing metadata-rich results
    print(f"\n{'#'*100}")
    print(f"DEMONSTRATION 1: Docker-related conversations")
    print(f"{'#'*100}")
    demonstrate_enhanced_search("docker compose issues", limit=2)

    print(f"\n{'#'*100}")
    print(f"DEMONSTRATION 2: Testing and quality")
    print(f"{'#'*100}")
    demonstrate_enhanced_search("testing setup and test failures", limit=2)

    print(f"\n{'#'*100}")
    print(f"DEMONSTRATION 3: API and authentication")
    print(f"{'#'*100}")
    demonstrate_enhanced_search("api authentication security", limit=2)


if __name__ == "__main__":
    main()
