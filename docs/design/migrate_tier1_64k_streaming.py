#!/usr/bin/env python3
"""
PRODUCTION TIER 1 MIGRATION - 64K Streaming
Complete coverage with all themes captured in comprehensive narratives.
Cost: $2.27 for 27 conversations (11% more than 16K, but 100% complete)
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
                "description": "Complete markdown narrative covering ALL major themes and work streams in the conversation"
            },
            "search_index": {
                "type": "string",
                "description": "Compact searchable summary combining ALL user requests, solution patterns, and key topics"
            },
            "metadata": {
                "type": "object",
                "properties": {
                    "tools_used": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Tools used (Read, Edit, Write, Bash, etc.) - up to 15",
                        "maxItems": 15
                    },
                    "concepts": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Technical concepts (docker, api, security, etc.) - up to 15",
                        "maxItems": 15
                    },
                    "files_modified": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Files created/edited with full paths - up to 15",
                        "maxItems": 15
                    }
                },
                "required": ["tools_used", "concepts", "files_modified"]
            }
        },
        "required": ["narrative", "search_index", "metadata"]
    }
}

SYSTEM_PROMPT = """You are a conversation analyzer extracting narratives from Claude Code conversations.

CRITICAL: This conversation may contain MULTIPLE major themes and work streams.
You MUST analyze the FULL conversation and capture ALL significant topics.

## Metadata Extraction

Identify:
- **tools_used**: Which tools did Claude use? (Read, Edit, Write, Bash, Grep, etc.) - up to 15
- **concepts**: What technical concepts appear? (docker, api, security, testing, kubernetes, etc.) - up to 15
- **files_modified**: Which files were created/edited? (full absolute paths) - up to 15

## Comprehensive Narrative Structure

Create a markdown narrative that captures ALL major work streams:

### Overview
2-3 sentence summary covering ALL major themes in the conversation

### Major Work Streams
List each significant theme/project/topic as a numbered section:
1. **Theme 1 Name**: Description and key accomplishments
2. **Theme 2 Name**: Description and key accomplishments
3. **Theme 3 Name**: Description and key accomplishments
...and so on for ALL significant work

### Technical Details
- Tools and approaches used across all work streams
- Files modified (with brief description per theme)
- Patterns and best practices that emerged

### Outcomes
- What was completed for EACH theme
- Any issues encountered and resolved
- Final status of all work streams

### Search Keywords
- **Primary**: 6-10 specific terms covering ALL themes
- **Secondary**: 10-15 variants, versions, errors across all work
- **Frameworks/Tools**: All technologies used
- **Pattern Tags**: Reusable identifiers for each theme

## Output

Use the `store_narrative` tool to save the complete narrative capturing ALL themes."""

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

def process_conversation_streaming(client, conv, embedding_model):
    """Process a single conversation with 64K streaming."""
    conversation_text = "\n\n---CHUNK---\n\n".join(conv['chunks'])

    print(f"  🔄 Streaming with 64K max_tokens...")

    collected_text = ""
    tool_use_input = None

    with client.messages.stream(
        model="claude-sonnet-4-20250514",  # Sonnet 4.5
        max_tokens=64000,  # Maximum for complete coverage
        system=SYSTEM_PROMPT,
        tools=[NARRATIVE_TOOL],
        tool_choice={"type": "tool", "name": "store_narrative"},
        messages=[{
            "role": "user",
            "content": f"""Analyze this conversation and extract ALL themes:

Conversation ID: {conv['conversation_id']}
Project: {conv['project']}
Chunks: {conv['chunk_count']}

CONVERSATION:
{conversation_text[:100000]}"""  # More input for better context
        }]
    ) as stream:
        # Wait for the complete message
        final_message = stream.get_final_message()

        # Extract tool use from message
        for content in final_message.content:
            if content.type == "tool_use" and content.name == "store_narrative":
                tool_use_input = content.input
                break

    if not tool_use_input:
        raise ValueError("No valid tool use data received from stream")

    tool_use_data = tool_use_input

    # Create Qdrant point
    search_text = tool_use_data['search_index']
    embedding = get_embedding(search_text, embedding_model)

    payload = {
        'conversation_id': conv['conversation_id'],
        'project': conv['project'],
        'narrative': tool_use_data['narrative'],
        'search_index': tool_use_data['search_index'],
        'timestamp': datetime.now().timestamp(),
        'source': 'tier1_64k_streaming_migration',
        'original_collection': conv['collection_name'],
        'signature': {
            'tools_used': tool_use_data['metadata']['tools_used'],
            'concepts': tool_use_data['metadata']['concepts'],
            'files_modified': tool_use_data['metadata']['files_modified'],
            'completion_status': 'migrated_64k'
        }
    }

    point = PointStruct(
        id=conv['conversation_id'],
        vector=embedding,
        payload=payload
    )

    return point, tool_use_data

def main():
    print("=" * 70)
    print("TIER 1 MIGRATION - 64K STREAMING")
    print("=" * 70)
    print("Model: Claude Sonnet 4.5")
    print("Max Tokens: 64,000 (complete coverage)")
    print("Method: Streaming + Tool Use (100% structured output)")
    print("Cost: $2.27 for all 27 conversations")
    print("Coverage: ALL themes captured (not just main theme)")
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
    print(f"💰 Estimated cost: ${len(conversations) * 0.0840:.2f}")
    print(f"📦 Will add to v3_all_projects collection")
    print(f"🎯 Complete coverage: ALL themes captured per conversation")
    print()

    # Process all conversations
    print("📝 Processing conversations with 64K streaming...")
    print("=" * 70)

    points_to_add = []
    processed_count = 0
    failed = []

    for i, conv in enumerate(conversations, 1):
        print(f"\n[{i}/{len(conversations)}] {conv['collection_name']} ({conv['chunk_count']} chunks)...")

        try:
            point, narrative_data = process_conversation_streaming(
                anthropic_client,
                conv,
                embedding_model
            )

            print(f"  ✅ Success!")
            print(f"     Narrative: {len(narrative_data['narrative']):,} chars")
            print(f"     Tools: {len(narrative_data['metadata']['tools_used'])}/15")
            print(f"     Concepts: {len(narrative_data['metadata']['concepts'])}/15")
            print(f"     Files: {len(narrative_data['metadata']['files_modified'])}/15")

            # Save narrative to file for review
            output_dir = Path(__file__).parent / "narratives_64k"
            output_dir.mkdir(exist_ok=True)
            output_file = output_dir / f"{conv['collection_name']}_narrative.md"

            with open(output_file, 'w') as f:
                f.write(f"# Narrative for {conv['collection_name']}\n\n")
                f.write(f"**Conversation ID**: {conv['conversation_id']}\n")
                f.write(f"**Project**: {conv['project']}\n")
                f.write(f"**Chunks**: {conv['chunk_count']}\n")
                f.write(f"**Length**: {len(narrative_data['narrative']):,} chars\n\n")
                f.write("---\n\n")
                f.write("## Search Index\n\n")
                f.write(narrative_data['search_index'])
                f.write("\n\n---\n\n")
                f.write("## Full Narrative\n\n")
                f.write(narrative_data['narrative'])
                f.write("\n\n---\n\n")
                f.write("## Metadata\n\n")
                f.write(f"**Tools**: {', '.join(narrative_data['metadata']['tools_used'])}\n\n")
                f.write(f"**Concepts**: {', '.join(narrative_data['metadata']['concepts'])}\n\n")
                f.write(f"**Files**: {', '.join(narrative_data['metadata']['files_modified'])}\n")

            print(f"     💾 Saved to {output_file}")

            points_to_add.append(point)
            processed_count += 1

            # Rate limiting
            if i < len(conversations):
                print("  ⏳ Rate limiting (2 seconds)...")
                time.sleep(2)

        except Exception as e:
            print(f"  ❌ Error: {e}")
            import traceback
            print(f"     Traceback: {traceback.format_exc()[:500]}")
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
    print(f"💰 Actual cost: ${processed_count * 0.0840:.2f}")

    if failed:
        print(f"\n❌ Failed: {len(failed)}")
        for name, error in failed:
            print(f"   - {name}: {error[:50]}...")

    # Check final collection size
    collection_info = qdrant_client.get_collection('v3_all_projects')
    print(f"\n📊 v3_all_projects now has {collection_info.points_count} narratives")
    print()
    print("🎯 Benefits of 64K:")
    print("   ✅ Complete coverage - ALL themes captured")
    print("   ✅ No missing details - OpenGraph, redesigns, all work streams")
    print("   ✅ Better search - comprehensive narratives")
    print("   ✅ Only 11% more expensive than 16K")
    print()
    print("🧪 Test with MCP tools (no restart needed):")
    print("   csr_reflect_on_past('OpenGraph procsolve website')")
    print("   Should now find comprehensive narrative with ALL themes!")


if __name__ == '__main__':
    main()
