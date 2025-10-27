#!/usr/bin/env python3
"""
TIER 1 migration using Tool Use for guaranteed structured JSON outputs.
"""

import os
import sys
import json
from pathlib import Path
from dotenv import load_dotenv
from datetime import datetime

load_dotenv()
sys.path.insert(0, str(Path(__file__).parent.parent.parent))

import anthropic
from qdrant_client import QdrantClient
from qdrant_client.models import PointStruct
from fastembed import TextEmbedding

def get_embedding(text: str, embedding_model) -> list:
    embeddings = list(embedding_model.embed([text]))
    return embeddings[0].tolist()

# Define the tool schema for narrative extraction
NARRATIVE_TOOL = {
    "name": "store_narrative",
    "description": "Store the extracted narrative and metadata from a conversation",
    "input_schema": {
        "type": "object",
        "properties": {
            "narrative": {
                "type": "string",
                "description": "Complete markdown narrative with sections: Search Summary, Problem-Solution Mapping, Technical Pattern (if applicable), Implementation Details, Validation & Outcome, Search Keywords"
            },
            "search_index": {
                "type": "string",
                "description": "Compact searchable summary combining user request, solution pattern, and key issues encountered"
            },
            "metadata": {
                "type": "object",
                "properties": {
                    "tools_used": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "List of tools used (Read, Edit, Write, Bash, etc.) - up to 10",
                        "maxItems": 10
                    },
                    "concepts": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Technical concepts (docker, api, security, testing, etc.) - up to 10",
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

def process_conversation_with_tool_use(client, conversation_text, model_id="claude-3-5-sonnet-20241022"):
    """Process a conversation using Tool Use for structured output."""

    response = client.messages.create(
        model=model_id,
        max_tokens=8192,
        system=SYSTEM_PROMPT,
        tools=[NARRATIVE_TOOL],
        tool_choice={"type": "tool", "name": "store_narrative"},  # Force tool use
        messages=[
            {
                "role": "user",
                "content": f"""Analyze this conversation and extract the narrative:

{conversation_text[:50000]}"""
            }
        ]
    )

    # Extract tool use from response
    for content in response.content:
        if content.type == "tool_use" and content.name == "store_narrative":
            return content.input  # This is guaranteed valid JSON!

    raise ValueError("No tool use found in response")

def main():
    print("=" * 70)
    print("TIER 1 MIGRATION WITH TOOL USE")
    print("=" * 70)
    print("Using structured outputs for 100% valid JSON")
    print()

    # Initialize clients
    anthropic_client = anthropic.Anthropic(api_key=os.getenv('ANTHROPIC_API_KEY'))
    qdrant_client = QdrantClient(url='http://localhost:6333')
    embedding_model = TextEmbedding(model_name='sentence-transformers/all-MiniLM-L6-v2')

    # Get TIER 1 conversations
    print("📊 Loading TIER 1 conversations...")
    collections = qdrant_client.get_collections().collections
    conv_cols = [c for c in collections if c.name.startswith('conv_') and c.name.endswith('_local')]

    conversations = []
    for col in conv_cols[:3]:  # Test with 3 first
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
                    'chunks': chunks
                })
                print(f"  ✓ {col.name}: {len(chunks)} chunks")

    print(f"\n✅ Loaded {len(conversations)} conversations for testing")

    # Process conversations
    print("\n📦 Processing with Tool Use...")
    points_to_add = []

    for i, conv in enumerate(conversations, 1):
        print(f"\n[{i}/{len(conversations)}] Processing {conv['collection_name']}...")

        try:
            conversation_text = "\n\n---CHUNK---\n\n".join(conv['chunks'])

            # Use Tool Use for structured output
            narrative_data = process_conversation_with_tool_use(
                anthropic_client,
                conversation_text,
                model_id="claude-3-5-sonnet-20241022"
            )

            print(f"  ✅ Narrative generated!")
            print(f"     Tools: {len(narrative_data['metadata']['tools_used'])}")
            print(f"     Concepts: {len(narrative_data['metadata']['concepts'])}")
            print(f"     Files: {len(narrative_data['metadata']['files_modified'])}")

            # Create Qdrant point
            search_text = narrative_data['search_index']
            embedding = get_embedding(search_text, embedding_model)

            payload = {
                'conversation_id': conv['conversation_id'],
                'project': conv['project'],
                'narrative': narrative_data['narrative'],
                'search_index': narrative_data['search_index'],
                'timestamp': datetime.now().timestamp(),
                'source': 'tier1_tool_use_migration',
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

            points_to_add.append(point)

        except Exception as e:
            print(f"  ❌ Error: {e}")

    # Add to Qdrant
    if points_to_add:
        print(f"\n📤 Adding {len(points_to_add)} points to v3_all_projects...")
        qdrant_client.upsert(
            collection_name='v3_all_projects',
            points=points_to_add
        )
        print(f"✅ Success!")

    # Summary
    collection_info = qdrant_client.get_collection('v3_all_projects')
    print(f"\n📊 v3_all_projects now has {collection_info.points_count} narratives")
    print("\n✅ Tool Use approach: 100% valid JSON, no parsing errors!")

if __name__ == '__main__':
    main()
