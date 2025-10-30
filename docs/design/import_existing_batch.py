#!/usr/bin/env python3
"""
Import existing batch results to Qdrant.
Batch ID: msgbatch_01QGo1y5maCUgqR7WWE1z2aT (27 conversations)
"""

import os
import sys
import json
import re
from pathlib import Path
from dotenv import load_dotenv
from datetime import datetime

load_dotenv()

sys.path.insert(0, str(Path(__file__).parent.parent.parent))

import anthropic
from qdrant_client import QdrantClient
from qdrant_client.models import PointStruct

# Import FastEmbed
from fastembed import TextEmbedding

def get_embedding(text: str, embedding_model) -> list:
    """Generate embedding for text."""
    embeddings = list(embedding_model.embed([text]))
    return embeddings[0].tolist()

def fix_json_response(content: str) -> str:
    """Fix Claude's backtick-based JSON responses."""
    # Try to extract JSON from markdown code fence if present
    if '```json' in content:
        json_start = content.find('```json') + 7
        json_end = content.find('```', json_start)
        content = content[json_start:json_end].strip()
    elif '```' in content:
        json_start = content.find('```') + 3
        json_end = content.find('```', json_start)
        content = content[json_start:json_end].strip()

    # Fix invalid JSON: replace backticks with escaped quotes for field values
    # Pattern: "field": `value` -> "field": "value with escaped newlines"
    content = re.sub(
        r':\s*`([^`]*)`',
        lambda m: f': "{m.group(1).replace(chr(10), "\\n").replace(chr(13), "").replace('"', '\\"')}"',
        content,
        flags=re.DOTALL
    )

    return content

def main():
    print("=" * 70)
    print("IMPORT EXISTING BATCH RESULTS")
    print("=" * 70)
    print(f"Batch ID: msgbatch_01QGo1y5maCUgqR7WWE1z2aT")
    print(f"Target: v3_all_projects collection")
    print()

    # Initialize clients
    anthropic_client = anthropic.Anthropic(api_key=os.getenv('ANTHROPIC_API_KEY'))
    qdrant_client = QdrantClient(url='http://localhost:6333')
    embedding_model = TextEmbedding(model_name='sentence-transformers/all-MiniLM-L6-v2')

    # Get TIER 1 conversation mapping
    print("📊 Loading TIER 1 conversation mapping...")
    collections = qdrant_client.get_collections().collections
    conv_cols = [c for c in collections if c.name.startswith('conv_') and c.name.endswith('_local')]

    conversations = []
    for col in conv_cols:
        base_id = col.name[5:-6]
        results = qdrant_client.scroll(
            collection_name=col.name,
            limit=1,
            with_payload=True
        )
        if results[0]:
            first_payload = results[0][0].payload
            conversation_id = first_payload.get('conversation_id', base_id)
            project = first_payload.get('project_name', 'unknown')
            conversations.append({
                'conversation_id': conversation_id,
                'project': project,
                'collection_name': col.name
            })

    print(f"✅ Loaded {len(conversations)} conversation mappings")

    # Retrieve batch results
    print("\n📥 Retrieving batch results...")
    results = []
    for result in anthropic_client.beta.messages.batches.results('msgbatch_01QGo1y5maCUgqR7WWE1z2aT'):
        if result.result.type == 'succeeded':
            results.append(result)

    print(f"✅ Retrieved {len(results)} successful results")

    # Process results
    print("\n📦 Processing narratives...")
    points_to_add = []
    processed_count = 0
    failed_ids = []

    for result in results:
        custom_id = result.custom_id
        try:
            response_content = result.result.message.content[0].text
            response_content = fix_json_response(response_content)
            narrative_data = json.loads(response_content)

            # Get original conversation data
            conv_idx = int(custom_id.split('_')[1]) - 1
            conv = conversations[conv_idx]

            # Create point
            search_text = narrative_data.get('search_index', narrative_data['narrative'][:1000])
            embedding = get_embedding(search_text, embedding_model)

            payload = {
                'conversation_id': conv['conversation_id'],
                'project': conv['project'],
                'narrative': narrative_data['narrative'],
                'search_index': narrative_data.get('search_index', ''),
                'timestamp': datetime.now().timestamp(),
                'source': 'tier1_migration',
                'original_collection': conv['collection_name']
            }

            if 'metadata' in narrative_data:
                metadata = narrative_data['metadata']
                payload['signature'] = {
                    'tools_used': metadata.get('tools_used', []),
                    'concepts': metadata.get('concepts', []),
                    'files_modified': metadata.get('files_modified', []),
                    'completion_status': 'migrated'
                }

            point = PointStruct(
                id=conv['conversation_id'],  # Use UUID directly
                vector=embedding,
                payload=payload
            )

            points_to_add.append(point)
            processed_count += 1

            if processed_count % 10 == 0:
                print(f"   Processed {processed_count}/{len(results)} narratives...")

        except Exception as e:
            failed_ids.append((custom_id, str(e)))
            print(f"   ⚠️  Error processing {custom_id}: {e}")

    # Add to Qdrant
    if points_to_add:
        print(f"\n📤 Adding {len(points_to_add)} points to v3_all_projects...")
        qdrant_client.upsert(
            collection_name='v3_all_projects',
            points=points_to_add
        )
        print(f"✅ Added {len(points_to_add)} narratives to Qdrant!")

    # Summary
    print("\n" + "=" * 70)
    print("IMPORT COMPLETE!")
    print("=" * 70)
    print(f"✅ Successfully processed: {processed_count}/{len(results)}")
    print(f"❌ Failed: {len(failed_ids)}/{len(results)}")

    if failed_ids:
        print("\nFailed IDs:")
        for custom_id, error in failed_ids:
            print(f"  - {custom_id}: {error[:50]}...")

    # Check final collection size
    collection_info = qdrant_client.get_collection('v3_all_projects')
    print(f"\n📊 v3_all_projects now has {collection_info.points_count} narratives")
    print(f"   (was 54, added {processed_count}, now {collection_info.points_count})")
    print()
    print("🎯 Test with MCP tools (no restart needed):")
    print("   csr_reflect_on_past('OpenGraph procsolve website')")


if __name__ == '__main__':
    main()
