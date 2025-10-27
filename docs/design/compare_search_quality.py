#!/usr/bin/env python3
"""Compare search quality: Current CSR system vs V3+SKILL_V2."""

import os
from pathlib import Path
from qdrant_client import QdrantClient
from fastembed import TextEmbedding

# Test queries
QUERIES = [
    "Next.js about page team member profile removal",
    "React component cleanup and refactoring",
    "ERR_CONNECTION_REFUSED localhost development server",
    "MultiEdit batch operations cascade updates",
    "Playwright testing navigation errors"
]


def search_v3_collection(query: str, qdrant_client: QdrantClient, embedding_model):
    """Search the V3+SKILL_V2 collection."""

    # Generate query embedding
    embeddings = list(embedding_model.embed([query]))
    query_vector = embeddings[0].tolist()

    # Search
    results = qdrant_client.search(
        collection_name="v3_test_procsolve",
        query_vector=query_vector,
        limit=3
    )

    return results


def display_comparison(query: str, v3_results):
    """Display side-by-side comparison."""

    print(f"\n{'='*100}")
    print(f"QUERY: {query}")
    print(f"{'='*100}")

    print(f"\n{'V3+SKILL_V2 RESULTS (New System)':^100}")
    print(f"{'-'*100}")

    for i, result in enumerate(v3_results, 1):
        print(f"\n[Rank {i}] Score: {result.score:.3f}")
        print(f"Conversation ID: {result.id}")

        # Show narrative excerpt
        narrative = result.payload.get('narrative', '')

        # Extract search summary section
        if "## Search Summary" in narrative:
            summary_start = narrative.index("## Search Summary") + len("## Search Summary")
            summary_end = narrative.find("##", summary_start)
            if summary_end == -1:
                summary_end = summary_start + 500
            summary = narrative[summary_start:summary_end].strip()
            print(f"\nSearch Summary:\n{summary[:300]}...")

        # Show keywords
        if "## Search Keywords" in narrative:
            keywords_start = narrative.index("## Search Keywords")
            keywords_section = narrative[keywords_start:keywords_start+400]
            print(f"\n{keywords_section}")

        print(f"\n{'-'*100}")

    print(f"\n{'COMPARISON WITH CSR MCP':^100}")
    print(f"{'-'*100}")
    print(f"To compare, run this CSR MCP search:")
    print(f"  mcp__claude-self-reflect__csr_reflect_on_past(")
    print(f"    query=\"{query}\",")
    print(f"    project=\"procsolve-website\",")
    print(f"    use_decay=0,")
    print(f"    limit=3")
    print(f"  )")
    print(f"{'-'*100}")


def main():
    """Run comparison tests."""

    print(f"\n{'='*100}")
    print(f"{'V3+SKILL_V2 SEARCH QUALITY TEST':^100}")
    print(f"{'='*100}")
    print(f"\nCollection: v3_test_procsolve (2 conversations)")
    print(f"Queries: {len(QUERIES)}")
    print(f"\nThis will show V3+SKILL_V2 results.")
    print(f"Then compare manually with CSR MCP searches on the same queries.")

    # Initialize
    print(f"\n🔧 Initializing...")
    qdrant_client = QdrantClient(url=os.getenv("QDRANT_URL", "http://localhost:6333"))
    embedding_model = TextEmbedding(model_name="sentence-transformers/all-MiniLM-L6-v2")
    print(f"   ✅ Ready")

    # Run queries
    for query in QUERIES:
        v3_results = search_v3_collection(query, qdrant_client, embedding_model)
        display_comparison(query, v3_results)
        print(f"\n")

    print(f"\n{'='*100}")
    print(f"ANALYSIS COMPLETE")
    print(f"{'='*100}")
    print(f"\nNext steps:")
    print(f"1. For each query, run the CSR MCP search shown above")
    print(f"2. Compare:")
    print(f"   - Relevance: Does V3 find the right conversation?")
    print(f"   - Detail: Does V3 narrative provide more context?")
    print(f"   - Searchability: Are V3 keywords more specific?")
    print(f"3. Document findings")


if __name__ == "__main__":
    main()
