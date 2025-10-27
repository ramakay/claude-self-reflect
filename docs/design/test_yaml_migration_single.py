#!/usr/bin/env python3
"""Test YAML front matter migration with ONE conversation"""

import os
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).parent.parent.parent))

# Import the main migration functions
from migrate_all_to_yaml_narratives import (
    process_conversation,
    get_embedding,
    anthropic,
    QdrantClient,
    TextEmbedding
)
from dotenv import load_dotenv

load_dotenv()

def main():
    print("=" * 70)
    print("TEST: Single Conversation YAML Migration")
    print("=" * 70)

    # Initialize clients
    anthropic_client = anthropic.Anthropic(api_key=os.getenv('ANTHROPIC_API_KEY'))
    qdrant_client = QdrantClient(url='http://localhost:6333')
    embedding_model = TextEmbedding(model_name='sentence-transformers/all-MiniLM-L6-v2')

    # Get test conversation (conv_35a2864c_local)
    results = qdrant_client.scroll(
        collection_name='conv_35a2864c_local',
        limit=100,
        with_payload=True
    )

    chunks = []
    first_payload = results[0][0].payload
    conversation_id = first_payload.get('conversation_id', '456f476d-2176-4bbb-b44e-9610fb0677b7')
    project = first_payload.get('project_name', 'unknown')

    for point in results[0]:
        chunk_text = point.payload.get('text', point.payload.get('content', ''))
        if chunk_text:
            chunks.append(chunk_text)

    test_conv = {
        'conversation_id': conversation_id,
        'project': project,
        'collection_name': 'conv_35a2864c_local',
        'chunks': chunks,
        'chunk_count': len(chunks),
        'source': 'tier1_chunks'
    }

    print(f"\n🧪 Testing with:")
    print(f"   ID: {conversation_id}")
    print(f"   Chunks: {len(chunks)}")
    print(f"   Project: {project}")
    print()

    # Process
    try:
        point, narrative_data = process_conversation(
            anthropic_client,
            test_conv,
            embedding_model
        )

        print("\n✅ SUCCESS!\n")
        print("=" * 70)
        print("YAML FRONT MATTER DATA:")
        print("=" * 70)
        import json
        print(json.dumps(narrative_data['yaml_frontmatter'], indent=2))

        print("\n" + "=" * 70)
        print("COMPLETE NARRATIVE (first 2000 chars):")
        print("=" * 70)
        print(point.payload['narrative'][:2000])
        print("\n...(truncated)\n")

        print("=" * 70)
        print("PAYLOAD SIGNATURE:")
        print("=" * 70)
        print(json.dumps(point.payload['signature'], indent=2))

        # Save to file for review
        output_file = Path(__file__).parent / "test_yaml_narrative.md"
        with open(output_file, 'w') as f:
            f.write(point.payload['narrative'])

        print(f"\n💾 Full narrative saved to: {output_file}")

        # Optionally add to Qdrant
        print("\n❓ Add to v3_all_projects? (This will REPLACE existing entry)")
        # For now, just show what would happen
        print(f"   Would upsert point with ID: {point.id}")
        print(f"   Collection: v3_all_projects")
        print("\n✅ Test complete! Review the output above before running full migration.")

    except Exception as e:
        print(f"\n❌ ERROR: {e}")
        import traceback
        traceback.print_exc()
        sys.exit(1)

if __name__ == '__main__':
    main()
