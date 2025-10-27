#!/usr/bin/env python3
"""
FINAL TIER 1 MIGRATION - Option 3
Process all 27 conversations with Claude Sonnet 4.5 + 16K + Tool Use
Cost: $2.09 for 100% consistent, high-quality narratives
"""

import os
import sys
import json
from pathlib import Path
from dotenv import load_dotenv
from datetime import datetime
import time

load_dotenv()
sys.path.insert(0, str(Path(__file__).parent.parent.parent))

import anthropic
from qdrant_client import QdrantClient
from qdrant_client.models import PointStruct
from fastembed import TextEmbedding

def get_embedding(text: str, embedding_model) -> list:
    embeddings = list(embedding_model.embed([text]))
    return embeddings[0].tolist()

# Tool schema for guaranteed structured output
NARRATIVE_TOOL = {
    "name": "store_narrative",
    "description": "Store the extracted narrative and metadata from a conversation",
    "input_schema": {
        "type": "object",
        "properties": {
            "narrative": {
                "type": "string",
                "description": "Complete markdown narrative with sections: Search Summary, Problem-Solution Mapping, Technical Pattern, Implementation Details, Validation & Outcome, Search Keywords"
            },
            "search_index": {
                "type": "string",
                "description": "Compact searchable summary combining user request, solution pattern, and key issues"
            },
            "metadata": {
                "type": "object",
                "properties": {
                    "tools_used": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Tools used (Read, Edit, Write, Bash, etc.) - up to 10",
                        "maxItems": 10
                    },
                    "concepts": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Technical concepts (docker, api, security, etc.) - up to 10",
                        "maxItems": 10
                    },
                    "files_modified": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Files created/edited with full paths - up to 10",
                        "maxItems": 10
                    }
                },
                "required": ["tools_used", "concepts", "files_modified"]
            }
        },
        "required": ["narrative", "search_index", "metadata"]
    }
}

SYSTEM_PROMPT = """You are a conversation analyzer extracting narratives from Claude Code conversations.

Analyze the FULL conversation to extract metadata FIRST, then write a comprehensive narrative.

## Metadata Extraction

Identify:
- **tools_used**: Which tools did Claude use? (Read, Edit, Write, Bash, Grep, etc.) - up to 10
- **concepts**: What technical concepts appear? (docker, api, security, testing, kubernetes, etc.) - up to 10
- **files_modified**: Which files were created/edited? (full absolute paths) - up to 10

## Narrative Structure

Create a markdown narrative with these sections:

### Search Summary
2-3 sentence overview: problem + solution + outcome

### Problem-Solution Mapping
- **Request**: User's original ask
- **Solution Type**: creation | edit | debugging | analysis
- **Tools Used**: List from metadata
- **Files Modified**: List from metadata with brief description

### Technical Pattern (if reusable)
- **Pattern Name**: Descriptive name
- **When to use**: Scenario
- **Steps**: 1, 2, 3...

### Implementation Details
- Approach and reasoning
- Specific commands/code used
- Multiple iterations if any

### Validation & Outcome
- Test results
- Build status
- Error recovery
- Final completion status

### Search Keywords
- **Primary**: 4-6 specific terms
- **Secondary**: 6-10 variants, versions, errors
- **Frameworks/Tools**: Technologies used
- **Pattern Tags**: Reusable identifiers

## Output

Use the `store_narrative` tool to save the extracted narrative and metadata."""

def get_tier1_conversations(qdrant_client):
    """Get all TIER 1 conversations."""
    print("\n📊 Discovering TIER 1 conversations...")
    print("=" * 70)

    collections = qdrant_client.get_collections().collections
    conv_cols = [c for c in collections if c.name.startswith('conv_') and c.name.endswith('_local')]

    print(f"Found {len(conv_cols)} conv_*_local collections")

    conversations = []
    for col in conv_cols:
        base_id = col.name[5:-6]
        results = qdrant_client.scroll(
            collection_name=col.name,
            limit=1000,
            with_payload=True
        )

        if results[0]:
            chunks = []
            first_payload = results[0][0].payload
            conversation_id = first_payload.get('conversation_id', base_id)
            project = first_payload.get('project_name', 'unknown')

            for point in results[0]:
                chunk_text = point.payload.get('text', point.payload.get('content', ''))
                if chunk_text:
                    chunks.append(chunk_text)

            if chunks:
                conversations.append({
                    'conversation_id': conversation_id,
                    'project': project,
                    'collection_name': col.name,
                    'chunks': chunks,
                    'chunk_count': len(chunks)
                })
                print(f"  ✓ {col.name}: {len(chunks)} chunks (project: {project})")

    print(f"\n✅ Found {len(conversations)} unique conversations in TIER 1")
    return conversations

def process_conversation(client, conv, embedding_model):
    """Process a single conversation with Tool Use."""
    conversation_text = "\n\n---CHUNK---\n\n".join(conv['chunks'])

    response = client.messages.create(
        model="claude-sonnet-4-20250514",  # Sonnet 4.5
        max_tokens=16384,  # Sweet spot: 2x better than 8K
        system=SYSTEM_PROMPT,
        tools=[NARRATIVE_TOOL],
        tool_choice={"type": "tool", "name": "store_narrative"},  # Force tool use
        messages=[
            {
                "role": "user",
                "content": f"""Analyze this conversation and extract the narrative:

Conversation ID: {conv['conversation_id']}
Project: {conv['project']}
Chunks: {conv['chunk_count']}

CONVERSATION:
{conversation_text[:50000]}"""
            }
        ]
    )

    # Extract tool use from response
    for content in response.content:
        if content.type == "tool_use" and content.name == "store_narrative":
            narrative_data = content.input

            # Create Qdrant point
            search_text = narrative_data['search_index']
            embedding = get_embedding(search_text, embedding_model)

            payload = {
                'conversation_id': conv['conversation_id'],
                'project': conv['project'],
                'narrative': narrative_data['narrative'],
                'search_index': narrative_data['search_index'],
                'timestamp': datetime.now().timestamp(),
                'source': 'tier1_final_migration_sonnet45',
                'original_collection': conv['collection_name'],
                'signature': {
                    'tools_used': narrative_data['metadata']['tools_used'],
                    'concepts': narrative_data['metadata']['concepts'],
                    'files_modified': narrative_data['metadata']['files_modified'],
                    'completion_status': 'migrated'
                }
            }

            point = PointStruct(
                id=conv['conversation_id'],
                vector=embedding,
                payload=payload
            )

            return point, narrative_data

    raise ValueError("No tool use found in response")

def main():
    print("=" * 70)
    print("FINAL TIER 1 MIGRATION - OPTION 3")
    print("=" * 70)
    print("Model: Claude Sonnet 4.5")
    print("Max Tokens: 16,384 (2x better quality)")
    print("Method: Tool Use (100% structured output)")
    print("Cost: $2.09 for all 27 conversations")
    print()

    # Initialize clients
    anthropic_client = anthropic.Anthropic(api_key=os.getenv('ANTHROPIC_API_KEY'))
    qdrant_client = QdrantClient(url='http://localhost:6333')
    embedding_model = TextEmbedding(model_name='sentence-transformers/all-MiniLM-L6-v2')

    # Get TIER 1 conversations
    conversations = get_tier1_conversations(qdrant_client)

    if not conversations:
        print("❌ No conversations found!")
        return

    # Confirm
    print(f"\n✅ Processing {len(conversations)} conversations")
    print(f"💰 Estimated cost: ${len(conversations) * 0.0773:.2f}")
    print(f"📦 Will add to v3_all_projects collection")
    print()

    # Process all conversations
    print("📝 Processing conversations with Tool Use...")
    print("=" * 70)

    points_to_add = []
    processed_count = 0
    failed = []

    for i, conv in enumerate(conversations, 1):
        print(f"\n[{i}/{len(conversations)}] {conv['collection_name']} ({conv['chunk_count']} chunks)...")

        try:
            point, narrative_data = process_conversation(
                anthropic_client,
                conv,
                embedding_model
            )

            print(f"  ✅ Success!")
            print(f"     Narrative: {len(narrative_data['narrative']):,} chars")
            print(f"     Tools: {len(narrative_data['metadata']['tools_used'])}/10")
            print(f"     Concepts: {len(narrative_data['metadata']['concepts'])}/10")
            print(f"     Files: {len(narrative_data['metadata']['files_modified'])}/10")

            points_to_add.append(point)
            processed_count += 1

            # Rate limiting
            if i < len(conversations):
                time.sleep(1)

        except Exception as e:
            print(f"  ❌ Error: {e}")
            failed.append((conv['collection_name'], str(e)))

    # Add to Qdrant
    if points_to_add:
        print(f"\n📤 Adding {len(points_to_add)} points to v3_all_projects...")
        qdrant_client.upsert(
            collection_name='v3_all_projects',
            points=points_to_add
        )
        print(f"✅ Added {len(points_to_add)} narratives!")

    # Summary
    print("\n" + "=" * 70)
    print("MIGRATION COMPLETE!")
    print("=" * 70)
    print(f"✅ Processed: {processed_count}/{len(conversations)}")
    print(f"💰 Actual cost: ${processed_count * 0.0773:.2f}")

    if failed:
        print(f"\n❌ Failed: {len(failed)}")
        for name, error in failed:
            print(f"   - {name}: {error[:50]}...")

    # Check final collection size
    collection_info = qdrant_client.get_collection('v3_all_projects')
    print(f"\n📊 v3_all_projects now has {collection_info.points_count} narratives")
    print()
    print("🎯 Test with MCP tools (no restart needed):")
    print("   csr_reflect_on_past('OpenGraph procsolve website')")


if __name__ == '__main__':
    main()
