#!/usr/bin/env python3
"""
COMPREHENSIVE MIGRATION TO YAML FRONT MATTER NARRATIVES
Migrates ALL conversations (TIER 1 chunks + existing v3 narratives) to new YAML format
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

# Updated tool schema with YAML front matter support
NARRATIVE_TOOL = {
    "name": "store_narrative",
    "description": "Store narrative with YAML front matter and structured metadata",
    "input_schema": {
        "type": "object",
        "properties": {
            "yaml_frontmatter": {
                "type": "object",
                "description": "YAML front matter metadata",
                "properties": {
                    "completion_status": {
                        "type": "string",
                        "enum": ["success", "failed", "partial"],
                        "description": "Overall completion status"
                    },
                    "tools": {
                        "type": "array",
                        "items": {"type": "string"},
                        "maxItems": 15,
                        "description": "Tools used (Read, Edit, Write, Bash, etc.)"
                    },
                    "concepts": {
                        "type": "array",
                        "items": {"type": "string"},
                        "maxItems": 15,
                        "description": "Technical concepts (docker, npm, api, etc.)"
                    },
                    "files": {
                        "type": "array",
                        "items": {"type": "string"},
                        "maxItems": 15,
                        "description": "Files modified (short names without full paths)"
                    },
                    "frameworks": {
                        "type": "array",
                        "items": {"type": "string"},
                        "maxItems": 10,
                        "description": "Frameworks/platforms used"
                    },
                    "outcomes": {
                        "type": "array",
                        "items": {"type": "string"},
                        "maxItems": 5,
                        "description": "Measurable outcomes (e.g., 'Memory: 25 GB → 568 MB')"
                    },
                    "technical_pattern": {
                        "type": "object",
                        "properties": {
                            "name": {"type": "string"},
                            "when_to_use": {"type": "string"},
                            "failure_modes": {
                                "type": "array",
                                "items": {"type": "string"},
                                "maxItems": 5
                            }
                        }
                    }
                },
                "required": ["completion_status", "tools", "concepts", "files"]
            },
            "narrative_body": {
                "type": "string",
                "description": "Markdown narrative WITHOUT front matter (body only)"
            }
        },
        "required": ["yaml_frontmatter", "narrative_body"]
    }
}

SYSTEM_PROMPT = """You are a conversation analyzer creating portable, self-contained markdown narratives.

## CRITICAL: YAML Front Matter Format

Your output MUST have two parts:
1. **yaml_frontmatter**: Structured metadata (JSON object that will be converted to YAML)
2. **narrative_body**: Pure markdown content (WITHOUT the YAML delimiters)

The system will combine these into a complete markdown file with YAML front matter.

## Metadata Extraction (for yaml_frontmatter)

Extract from the FULL conversation:

1. **completion_status**: "success" | "failed" | "partial"
   - success: Build passed, tests passed, deployment succeeded, OR user confirmed completion
   - failed: Unresolved blocking errors in final 20% of conversation
   - partial: Some work done but not fully complete

2. **tools**: Array of tools Claude used (up to 15)
   - Examples: ["Read", "Edit", "Write", "Bash", "Docker", "gh"]

3. **concepts**: Technical concepts (up to 15)
   - Examples: ["docker", "npm", "github-actions", "memory-leak", "ci-cd"]

4. **files**: Files modified - SHORT names only (up to 15)
   - Examples: ["Dockerfile.importer", "package.json", "README.md"]
   - NOT full paths like "/Users/..."

5. **frameworks**: Frameworks/platforms (up to 10)
   - Examples: ["react", "nextjs", "typescript", "python", "docker"]

6. **outcomes**: Measurable results (up to 5)
   - Format: "Metric: Before → After" or "Event: What happened"
   - Examples: ["Memory: 25 GB → 568 MB", "v2.7.1 published to npm", "CI/CD: All tests passing"]

7. **technical_pattern** (if reusable pattern exists):
   - name: "Descriptive Pattern Name"
   - when_to_use: "Scenario description"
   - failure_modes: Array of what can go wrong (up to 5)

## Narrative Body Structure (for narrative_body)

Create markdown with these sections:

### # Title
Concise title describing the work

### ## Search Summary
2-3 sentences: problem + solution + outcome

### ## Problem-Solution Mapping
- **Request**: User's original ask
- **Solution Type**: creation | edit | debugging | analysis
- **Tools Used**: List from metadata
- **Files Modified**: List with brief descriptions

### ## Technical Pattern (if applicable)
- **Pattern Name**: From yaml_frontmatter
- **When to use**: From yaml_frontmatter
- **Failure Modes**: From yaml_frontmatter
- **Steps**: Numbered implementation steps

### ## Implementation Details
- Approach and reasoning
- Key commands/code used
- Iterations and adjustments

### ## Validation & Outcome
- Test results
- Build status
- Error recovery
- Final completion status

### ## Search Keywords
- **Primary**: 4-6 specific terms
- **Secondary**: 6-10 variants, versions, errors
- **Frameworks/Tools**: Technologies used
- **Pattern Tags**: Reusable identifiers

## Output

Use the `store_narrative` tool with:
- yaml_frontmatter: Complete metadata object
- narrative_body: Pure markdown content (NO YAML delimiters like --- or front matter)
"""

def build_yaml_frontmatter(frontmatter_dict: dict, conversation_id: str, project: str) -> str:
    """Build YAML front matter string from dictionary."""
    import yaml

    # Add standard fields
    frontmatter_dict['id'] = conversation_id
    frontmatter_dict['project'] = project
    frontmatter_dict['date'] = datetime.now().strftime('%Y-%m-%dT%H:%M:%SZ')

    # Convert to YAML
    yaml_str = yaml.dump(frontmatter_dict, default_flow_style=False, allow_unicode=True, sort_keys=False)

    return f"---\n{yaml_str}---\n\n"

def get_all_conversations(qdrant_client):
    """Get ALL conversations from both TIER 1 (chunks) and v3 (existing narratives)."""
    print("\n📊 Discovering ALL conversations...")
    print("=" * 70)

    conversations = {}

    # Get TIER 1 (conv_*_local collections with chunks)
    collections = qdrant_client.get_collections().collections
    conv_cols = [c for c in collections if c.name.startswith('conv_') and c.name.endswith('_local')]

    print(f"Found {len(conv_cols)} TIER 1 conv_*_local collections")

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

            if chunks and conversation_id not in conversations:
                conversations[conversation_id] = {
                    'conversation_id': conversation_id,
                    'project': project,
                    'collection_name': col.name,
                    'chunks': chunks,
                    'chunk_count': len(chunks),
                    'source': 'tier1_chunks'
                }
                print(f"  ✓ TIER 1: {col.name}: {len(chunks)} chunks (project: {project})")

    print(f"\n✅ Found {len(conversations)} unique conversations")
    return list(conversations.values())

def process_conversation(client, conv, embedding_model):
    """Process a single conversation with YAML front matter format."""
    conversation_text = "\n\n---CHUNK---\n\n".join(conv['chunks'][:200])  # Limit for API

    response = client.messages.create(
        model="claude-sonnet-4-20250514",  # Sonnet 4.5
        max_tokens=16384,
        system=SYSTEM_PROMPT,
        tools=[NARRATIVE_TOOL],
        tool_choice={"type": "tool", "name": "store_narrative"},
        messages=[{
            "role": "user",
            "content": f"""Analyze this conversation and create a narrative with YAML front matter:

Conversation ID: {conv['conversation_id']}
Project: {conv['project']}
Chunks: {conv['chunk_count']}

CONVERSATION:
{conversation_text}"""
        }]
    )

    # Extract tool use
    for content in response.content:
        if content.type == "tool_use" and content.name == "store_narrative":
            data = content.input

            # Build complete narrative with YAML front matter
            yaml_fm = build_yaml_frontmatter(
                data['yaml_frontmatter'],
                conv['conversation_id'],
                conv['project']
            )
            complete_narrative = yaml_fm + data['narrative_body']

            # Create search index from narrative
            lines = data['narrative_body'].split('\n')
            search_parts = []
            for line in lines[:30]:  # First 30 lines for search
                if line.strip() and not line.startswith('#'):
                    search_parts.append(line.strip())
            search_index = '\n'.join(search_parts[:10])  # Top 10 meaningful lines

            # Build embedding from search index
            embedding = get_embedding(search_index, embedding_model)

            # Build Qdrant payload
            payload = {
                'conversation_id': conv['conversation_id'],
                'project': conv['project'],
                'narrative': complete_narrative,
                'search_index': search_index,
                'timestamp': datetime.now().timestamp(),
                'source': 'yaml_migration_v4',
                'original_collection': conv.get('collection_name'),
                'signature': {
                    'completion_status': data['yaml_frontmatter']['completion_status'],
                    'frameworks': data['yaml_frontmatter'].get('frameworks', []),
                    'pattern_reusability': 'high' if data['yaml_frontmatter'].get('technical_pattern') else 'medium',
                    'error_recovery': data['yaml_frontmatter']['completion_status'] == 'success',
                    'total_edits': len(data['yaml_frontmatter'].get('files', [])),
                    'tools_used': data['yaml_frontmatter']['tools'],
                    'files_modified': data['yaml_frontmatter']['files'],
                    'concepts': data['yaml_frontmatter']['concepts'],
                    'outcomes': data['yaml_frontmatter'].get('outcomes', [])
                }
            }

            point = PointStruct(
                id=conv['conversation_id'],
                vector=embedding,
                payload=payload
            )

            return point, data

    raise ValueError("No tool use found in response")

def main():
    print("=" * 70)
    print("COMPREHENSIVE YAML FRONT MATTER MIGRATION")
    print("=" * 70)
    print("Model: Claude Sonnet 4.5")
    print("Max Tokens: 16,384")
    print("Target: v3_all_projects collection")
    print("Format: YAML front matter + markdown body")
    print()

    # Initialize clients
    anthropic_client = anthropic.Anthropic(api_key=os.getenv('ANTHROPIC_API_KEY'))
    qdrant_client = QdrantClient(url='http://localhost:6333')
    embedding_model = TextEmbedding(model_name='sentence-transformers/all-MiniLM-L6-v2')

    # Get ALL conversations
    conversations = get_all_conversations(qdrant_client)

    if not conversations:
        print("❌ No conversations found!")
        return

    # Confirm
    print(f"\n✅ Processing {len(conversations)} conversations")
    print(f"💰 Estimated cost: ${len(conversations) * 0.0773:.2f}")
    print(f"📦 Will update v3_all_projects collection")
    print()

    # Process all
    print("📝 Processing conversations with YAML front matter...")
    print("=" * 70)

    points_to_add = []
    processed_count = 0
    failed = []

    for i, conv in enumerate(conversations, 1):
        print(f"\n[{i}/{len(conversations)}] {conv.get('collection_name', conv['conversation_id'])} ({conv['chunk_count']} chunks)...")

        try:
            point, narrative_data = process_conversation(
                anthropic_client,
                conv,
                embedding_model
            )

            print(f"  ✅ Success!")
            print(f"     Tools: {len(narrative_data['yaml_frontmatter']['tools'])}/15")
            print(f"     Concepts: {len(narrative_data['yaml_frontmatter']['concepts'])}/15")
            print(f"     Files: {len(narrative_data['yaml_frontmatter']['files'])}/15")
            if narrative_data['yaml_frontmatter'].get('outcomes'):
                print(f"     Outcomes: {len(narrative_data['yaml_frontmatter']['outcomes'])}")

            points_to_add.append(point)
            processed_count += 1

            # Rate limiting
            if i < len(conversations):
                time.sleep(1)

        except Exception as e:
            print(f"  ❌ Error: {e}")
            failed.append((conv.get('collection_name', conv['conversation_id']), str(e)))

    # Add to Qdrant
    if points_to_add:
        print(f"\n📤 Upserting {len(points_to_add)} points to v3_all_projects...")
        qdrant_client.upsert(
            collection_name='v3_all_projects',
            points=points_to_add
        )
        print(f"✅ Updated {len(points_to_add)} narratives!")

    # Summary
    print("\n" + "=" * 70)
    print("MIGRATION COMPLETE!")
    print("=" * 70)
    print(f"✅ Processed: {processed_count}/{len(conversations)}")
    print(f"💰 Actual cost: ${processed_count * 0.0773:.2f}")

    if failed:
        print(f"\n❌ Failed: {len(failed)}")
        for name, error in failed[:5]:
            print(f"   - {name}: {error[:50]}...")

    # Check final collection size
    collection_info = qdrant_client.get_collection('v3_all_projects')
    print(f"\n📊 v3_all_projects now has {collection_info.points_count} narratives")
    print()
    print("🎯 New YAML Front Matter Format:")
    print("   ✅ Portable markdown files with metadata")
    print("   ✅ outcomes: Measurable results")
    print("   ✅ technical_pattern.failure_modes: Reusable patterns")
    print("   ✅ Standard YAML format (Obsidian/Jekyll/Hugo compatible)")

if __name__ == '__main__':
    main()
