#!/usr/bin/env python3
"""
Migrate TIER 1 (conv_*_local) collections to narrative format.

Processes 31 unique conversations from old format into v3_all_projects.
Cost: $3.10 (31 conversations × $0.10 batch API pricing)
"""

import os
import sys
import json
from pathlib import Path
from dotenv import load_dotenv
import time
from datetime import datetime

load_dotenv()

sys.path.insert(0, str(Path(__file__).parent.parent.parent))

import anthropic
from qdrant_client import QdrantClient
from qdrant_client.models import PointStruct

# Import FastEmbed
try:
    from fastembed import TextEmbedding
    FASTEMBED_AVAILABLE = True
except ImportError:
    FASTEMBED_AVAILABLE = False
    print("⚠️  FastEmbed not available")
    sys.exit(1)


def get_embedding(text: str, embedding_model) -> list:
    """Generate embedding for text."""
    embeddings = list(embedding_model.embed([text]))
    return embeddings[0].tolist()


def get_tier1_conversations(qdrant_client):
    """Get unique conversations from TIER 1 (conv_*_local collections)."""
    print("\n📊 Discovering TIER 1 conversations...")
    print("=" * 70)

    collections = qdrant_client.get_collections().collections
    conv_cols = [c for c in collections if c.name.startswith('conv_') and c.name.endswith('_local')]

    print(f"Found {len(conv_cols)} conv_*_local collections")

    conversations = []

    for col in conv_cols:
        # Extract base conversation ID
        base_id = col.name[5:-6]  # Remove 'conv_' prefix and '_local' suffix

        # Get conversation data from collection
        results = qdrant_client.scroll(
            collection_name=col.name,
            limit=1000,  # Get all chunks
            with_payload=True
        )

        if not results[0]:
            continue

        # Reconstruct conversation from chunks
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


def create_batch_requests(conversations):
    """Create batch API requests for narrative generation."""
    print("\n📝 Creating batch requests...")
    print("=" * 70)

    requests = []

    for i, conv in enumerate(conversations, 1):
        # Reconstruct conversation text from chunks
        full_text = "\n\n---CHUNK---\n\n".join(conv['chunks'])

        # Create system prompt for V3+SKILL_V2+Metadata
        system_prompt = """You are a conversation analyzer. Extract a comprehensive narrative from this Claude Code conversation.

CRITICAL: Analyze the FULL conversation to extract metadata BEFORE writing the narrative.

## Step 1: Extract Metadata (analyze entire conversation first)

Identify:
- tools_used: Which tools did Claude use? (Read, Edit, Write, Bash, etc.) - up to 10
- concepts: What technical concepts appear? (docker, api, security, testing, etc.) - up to 10
- files_modified: Which files were created/edited? (full paths) - up to 10

## Step 2: Generate Narrative

Create a comprehensive markdown narrative with these sections:

### ## Search Summary (2-3 sentences)
Concise overview: problem + solution + outcome

### ## Problem-Solution Mapping
**Request**: User's original ask
**Solution Type**: creation | edit | debugging | analysis
**Tools Used**: List from metadata
**Files Modified**: List from metadata with brief description

### ## Technical Pattern (if reusable)
Pattern Name: [descriptive name]
When to use: [scenario]
Steps: [1, 2, 3...]

### ## Implementation Details
- Approach and reasoning
- Specific commands/code used
- Multiple iterations if any

### ## Validation & Outcome
- Test results
- Build status
- Error recovery
- Final completion status

### ## Search Keywords
**Primary**: 4-6 specific terms
**Secondary**: 6-10 variants, versions, errors
**Frameworks/Tools**: Technologies used
**Pattern Tags**: Reusable identifiers

## Output Format

Return a JSON object:
{
  "narrative": "Complete markdown narrative",
  "search_index": "Compact searchable summary (user request + solution pattern + issues)",
  "metadata": {
    "tools_used": ["Read", "Edit", ...],
    "concepts": ["docker", "api", ...],
    "files_modified": ["path/to/file", ...]
  }
}
"""

        user_prompt = f"""Analyze this conversation and create a comprehensive narrative:

Conversation ID: {conv['conversation_id']}
Project: {conv['project']}
Chunks: {conv['chunk_count']}

CONVERSATION:
{full_text[:50000]}  # Limit to avoid token limits

Generate the narrative following the system instructions."""

        requests.append({
            "custom_id": f"tier1_{i:03d}_{conv['conversation_id'][:8]}",
            "params": {
                "model": "claude-3-5-sonnet-20241022",
                "max_tokens": 4096,  # Reduced for narrative efficiency
                "system": system_prompt,
                "messages": [
                    {
                        "role": "user",
                        "content": user_prompt
                    }
                ]
            }
        })

        if i % 10 == 0:
            print(f"  Created {i}/{len(conversations)} requests...")

    print(f"✅ Created {len(requests)} batch requests")
    return requests


def main():
    """Main migration process."""
    print("=" * 70)
    print("TIER 1 → NARRATIVE MIGRATION")
    print("=" * 70)
    print(f"Budget: $3.10 (31 conversations)")
    print(f"Target: v3_all_projects collection")
    print()

    # Initialize clients
    anthropic_client = anthropic.Anthropic(api_key=os.getenv('ANTHROPIC_API_KEY'))
    qdrant_client = QdrantClient(url='http://localhost:6333')
    embedding_model = TextEmbedding(model_name='sentence-transformers/all-MiniLM-L6-v2')

    # Step 1: Get TIER 1 conversations
    conversations = get_tier1_conversations(qdrant_client)

    if not conversations:
        print("❌ No conversations found in TIER 1!")
        return

    # Confirm before proceeding
    estimated_cost = len(conversations) * 0.10
    print(f"\n✅ Processing {len(conversations)} conversations")
    print(f"💰 Estimated cost: ${estimated_cost:.2f}")
    print(f"📦 Will add to v3_all_projects collection")
    print()

    # Auto-confirm (user already approved)
    print("✅ Auto-confirmed - proceeding with migration...")

    # Step 2: Create batch requests
    requests = create_batch_requests(conversations)

    # Step 3: Write batch file
    batch_file = Path(__file__).parent / 'tier1_batch_requests.jsonl'
    print(f"\n📝 Writing batch file: {batch_file}")

    with open(batch_file, 'w') as f:
        for req in requests:
            f.write(json.dumps(req) + '\n')

    print(f"✅ Wrote {len(requests)} requests to {batch_file}")

    # Step 4: Upload batch
    print("\n📤 Uploading batch to Anthropic...")

    # Read JSONL as list of dictionaries
    batch_requests = []
    with open(batch_file, 'r') as f:
        for line in f:
            batch_requests.append(json.loads(line))

    batch_upload = anthropic_client.beta.messages.batches.create(
        requests=batch_requests
    )

    batch_id = batch_upload.id
    print(f"✅ Batch created: {batch_id}")
    print(f"   Status: {batch_upload.processing_status}")

    # Step 5: Wait for completion
    print("\n⏳ Waiting for batch processing...")
    print("   This may take 10-30 minutes for 31 conversations...")

    while True:
        batch_status = anthropic_client.beta.messages.batches.retrieve(batch_id)

        status = batch_status.processing_status
        print(f"   Status: {status}")

        if hasattr(batch_status, 'request_counts'):
            counts = batch_status.request_counts
            print(f"   Progress: {counts.succeeded}/{counts.processing + counts.succeeded + counts.errored} requests")

        if status == 'ended':
            print("✅ Batch processing completed!")
            break
        elif status in ['failed', 'expired']:
            print(f"❌ Batch {status}!")
            return

        time.sleep(30)  # Check every 30 seconds

    # Step 6: Retrieve results
    print("\n📥 Retrieving results...")

    results = []
    for result in anthropic_client.beta.messages.batches.results(batch_id):
        if result.result.type == 'succeeded':
            results.append(result)

    print(f"✅ Retrieved {len(results)} successful results")

    # Step 7: Import to Qdrant
    print("\n📦 Importing to Qdrant...")

    points_to_add = []
    processed_count = 0

    for result in results:
        custom_id = result.custom_id  # Get ID first for error handling
        try:
            # Extract narrative from response
            response_content = result.result.message.content[0].text

            # Try to extract JSON from markdown code fence if present
            if '```json' in response_content:
                json_start = response_content.find('```json') + 7
                json_end = response_content.find('```', json_start)
                response_content = response_content[json_start:json_end].strip()
            elif '```' in response_content:
                json_start = response_content.find('```') + 3
                json_end = response_content.find('```', json_start)
                response_content = response_content[json_start:json_end].strip()

            # Fix invalid JSON: replace backticks with escaped quotes for field values
            # Pattern: "field": `value` or "field": `value`,
            import re
            response_content = re.sub(
                r':\s*`([^`]*)`',
                lambda m: f': "{m.group(1).replace(chr(10), "\\n").replace('"', '\\"')}"',
                response_content,
                flags=re.DOTALL
            )

            # Parse JSON response
            narrative_data = json.loads(response_content)

            # Get original conversation data
            conv_idx = int(custom_id.split('_')[1]) - 1
            conv = conversations[conv_idx]

            # Create point for Qdrant
            search_text = narrative_data.get('search_index', narrative_data['narrative'][:1000])
            embedding = get_embedding(search_text, embedding_model)

            # Prepare payload
            payload = {
                'conversation_id': conv['conversation_id'],
                'project': conv['project'],
                'narrative': narrative_data['narrative'],
                'search_index': narrative_data.get('search_index', ''),
                'timestamp': datetime.now().timestamp(),
                'source': 'tier1_migration',
                'original_collection': conv['collection_name']
            }

            # Add metadata if present
            if 'metadata' in narrative_data:
                metadata = narrative_data['metadata']
                payload['signature'] = {
                    'tools_used': metadata.get('tools_used', []),
                    'concepts': metadata.get('concepts', []),
                    'files_modified': metadata.get('files_modified', []),
                    'completion_status': 'migrated'
                }

            point = PointStruct(
                id=conv['conversation_id'],  # Use UUID directly without prefix
                vector=embedding,
                payload=payload
            )

            points_to_add.append(point)
            processed_count += 1

            if processed_count % 10 == 0:
                print(f"   Processed {processed_count}/{len(results)} narratives...")

        except Exception as e:
            print(f"   ⚠️  Error processing {custom_id}: {e}")

    # Add to Qdrant
    if points_to_add:
        print(f"\n📤 Adding {len(points_to_add)} points to v3_all_projects...")

        qdrant_client.upsert(
            collection_name='v3_all_projects',
            points=points_to_add
        )

        print(f"✅ Added {len(points_to_add)} narratives to Qdrant!")

    # Step 8: Summary
    print("\n" + "=" * 70)
    print("MIGRATION COMPLETE!")
    print("=" * 70)
    print(f"✅ Processed: {processed_count} conversations")
    print(f"✅ Added to: v3_all_projects collection")
    print(f"💰 Actual cost: ${processed_count * 0.10:.2f}")
    print()

    # Check final collection size
    collection_info = qdrant_client.get_collection('v3_all_projects')
    print(f"📊 v3_all_projects now has {collection_info.points_count} narratives")
    print(f"   (was 54, added {processed_count}, now {collection_info.points_count})")
    print()
    print("🎯 Test with MCP tools (no restart needed):")
    print("   csr_reflect_on_past('OpenGraph procsolve website')")


if __name__ == '__main__':
    main()
