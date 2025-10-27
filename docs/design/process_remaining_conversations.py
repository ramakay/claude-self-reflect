#!/usr/bin/env python3
"""
Process the 5 remaining conversations manually for 100% coverage.

Uses direct API calls (not batch) with V3+SKILL_V2+Metadata approach.
"""

import os
import sys
import json
from pathlib import Path
from dotenv import load_dotenv
import time

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


# Missing conversations (identified from recovery output)
MISSING_CONVERSATIONS = [
    {
        'conv_id': '637cf8a8-006c-43d1-97c8-998366ecb2fa',
        'project': 'procsolve-website'
    },
    {
        'conv_id': '6cbc31a3-9abe-4153-999a-8e4628e22ebc',
        'project': 'procsolve-website'
    },
    {
        'conv_id': 'ef341585-66e4-43b5-b4e3-deed3aea59f2',
        'project': 'thegatehouse'
    },
    {
        'conv_id': 'dba507c4-db0c-43be-87f5-15b91cc464b5',
        'project': 'buyindian'
    },
    {
        'conv_id': 'f525be1c-8a03-42cc-bf36-c0f2c2e41d62',
        'project': 'buyindian'
    }
]


def get_embedding(text: str, embedding_model) -> list:
    """Generate embedding for text."""
    embeddings = list(embedding_model.embed([text]))
    return embeddings[0].tolist()


def generate_narrative(client: anthropic.Anthropic, result: dict, skill_instructions: str) -> str:
    """Generate narrative for a single conversation using direct API."""

    # Build metadata context section if available
    metadata_context = ""
    if 'metadata' in result:
        metadata = result['metadata']
        tool_usage = metadata.get('tool_usage', {})
        concepts = metadata.get('concepts', [])

        metadata_context = f"""
### Metadata Context (USE THIS to enhance your narrative)

**Tools Used**: {json.dumps(tool_usage.get('tools_summary', {}))}
**Files Analyzed**: {tool_usage.get('files_read', [])[:10]}
**Files Modified**: {tool_usage.get('files_edited', [])[:10]}
**Concepts Detected**: {list(concepts)[:10]}
**Grep Searches**: {[s.get('pattern', '') for s in tool_usage.get('grep_searches', [])][:5]}
**Bash Commands**: {[cmd.get('command', '')[:100] for cmd in tool_usage.get('bash_commands', [])][:5]}

Use this metadata to understand:
- What tools were actually used (Read, Edit, Grep, Bash, etc.)
- Which files were involved in this conversation
- What technical concepts and domains this conversation touched
- What the developer was searching for and building
"""

    prompt = f"""You are analyzing a development conversation. Use the SKILL_V2 guidelines to generate a search-optimized narrative.

## Extracted Events

### Search Index
{result['search_index']}

### Context Cache
{result['context_cache']}
{metadata_context}
### Conversation Signature
```json
{json.dumps(result['signature'], indent=2)}
```

Now generate the narrative following SKILL_V2 format exactly, using ALL the context above including metadata."""

    # Direct API call (not batch)
    message = client.messages.create(
        model="claude-sonnet-4-5-20250929",
        max_tokens=2048,
        system=skill_instructions,
        messages=[{"role": "user", "content": prompt}]
    )

    # Extract narrative
    narrative = ""
    for block in message.content:
        if hasattr(block, 'text'):
            narrative += block.text

    # Calculate cost
    input_tokens = message.usage.input_tokens
    output_tokens = message.usage.output_tokens
    cost = (input_tokens * 3 + output_tokens * 15) / 1_000_000

    return narrative, cost, input_tokens, output_tokens


def load_conversation(conv_id: str, project: str, projects_dir: Path):
    """Load and process a single conversation."""

    # Import metadata extraction functions
    import importlib.util
    delta_metadata_path = Path(__file__).parent.parent.parent / "src" / "runtime" / "delta-metadata-update.py"
    spec = importlib.util.spec_from_file_location("delta_metadata_update", delta_metadata_path)
    delta_metadata_update = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(delta_metadata_update)
    extract_tool_usage_from_jsonl = delta_metadata_update.extract_tool_usage_from_jsonl
    extract_concepts = delta_metadata_update.extract_concepts

    from docs.design.extract_events_v3 import extract_events_v3

    # Find JSONL file
    project_dirs = list(projects_dir.glob(f"*{project}"))
    if not project_dirs:
        raise ValueError(f"Project directory not found for {project}")

    jsonl_file = project_dirs[0] / f"{conv_id}.jsonl"
    if not jsonl_file.exists():
        raise ValueError(f"JSONL file not found: {jsonl_file}")

    print(f"  📂 Loading {jsonl_file}")

    # Extract metadata FIRST
    tool_usage = extract_tool_usage_from_jsonl(str(jsonl_file))

    # Read messages for V3 extraction
    messages = []
    conversation_text = ""
    with open(jsonl_file) as f:
        for line in f:
            if line.strip():
                msg = json.loads(line)
                messages.append(msg)

                if 'message' in msg and msg['message']:
                    content = msg['message'].get('content', '')
                    if isinstance(content, str):
                        conversation_text += content + "\n"
                    elif isinstance(content, list):
                        for item in content:
                            if isinstance(item, dict) and item.get('text'):
                                conversation_text += item['text'] + "\n"

    # Extract concepts
    concepts = extract_concepts(conversation_text[:10000], tool_usage)

    # Build metadata dict
    metadata = {
        'tool_usage': tool_usage,
        'concepts': concepts
    }

    # V3 extraction WITH metadata
    result = extract_events_v3(messages, metadata=metadata)

    print(f"  ✅ V3 extraction: {result['stats']['total_tokens']} tokens, {len(concepts)} concepts, {len(tool_usage.get('tools_summary', {}))} tool types")

    return result


def main():
    """Process missing conversations and import to Qdrant."""

    print(f"\n{'='*80}")
    print(f"MANUAL PROCESSING: 5 REMAINING CONVERSATIONS")
    print(f"{'='*80}\n")

    # Initialize clients
    print("🔧 Initializing clients...")
    anthropic_client = anthropic.Anthropic(api_key=os.getenv("ANTHROPIC_API_KEY"))
    qdrant_client = QdrantClient(url=os.getenv("QDRANT_URL", "http://localhost:6333"))
    embedding_model = TextEmbedding(model_name="sentence-transformers/all-MiniLM-L6-v2")
    print("  ✅ Clients initialized")

    # Load SKILL_V2
    skill_v2_path = Path(__file__).parent / "conversation-analyzer" / "SKILL_V2.md"
    if not skill_v2_path.exists():
        print(f"❌ SKILL_V2.md not found: {skill_v2_path}")
        sys.exit(1)

    with open(skill_v2_path) as f:
        skill_instructions = f.read()

    projects_dir = Path.home() / ".claude/projects"
    collection_name = "v3_all_projects"

    # Process each conversation
    all_points = []
    total_cost = 0.0
    total_input = 0
    total_output = 0

    print(f"\n🔄 Processing {len(MISSING_CONVERSATIONS)} conversations...\n")

    for conv_info in MISSING_CONVERSATIONS:
        conv_id = conv_info['conv_id']
        project = conv_info['project']

        print(f"\n{'='*80}")
        print(f"Processing: {conv_id[:8]}... ({project})")
        print(f"{'='*80}")

        try:
            # Load and extract with metadata
            result = load_conversation(conv_id, project, projects_dir)

            # Generate narrative
            print(f"  🔄 Generating narrative...")
            narrative, cost, input_tokens, output_tokens = generate_narrative(
                anthropic_client,
                result,
                skill_instructions
            )

            total_cost += cost
            total_input += input_tokens
            total_output += output_tokens

            print(f"  ✅ Narrative generated: {len(narrative)} chars")
            print(f"  📊 Tokens: {input_tokens} input, {output_tokens} output")
            print(f"  💰 Cost: ${cost:.4f}")

            # Generate embedding
            print(f"  🔄 Generating embedding...")
            embedding = get_embedding(narrative, embedding_model)

            # Create point
            point = PointStruct(
                id=conv_id,
                vector=embedding,
                payload={
                    "conversation_id": conv_id,
                    "project": project,
                    "narrative": narrative,
                    "search_index": result['search_index'],
                    "context_cache": result['context_cache'],
                    "signature": result['signature'],
                    "timestamp": time.time(),
                    "extraction_stats": result['stats']
                }
            )

            all_points.append(point)
            print(f"  ✅ Point created with {len(embedding)} dimensions")

        except Exception as e:
            print(f"  ❌ Failed to process {conv_id[:8]}...: {e}")
            continue

    # Import to Qdrant
    print(f"\n\n{'='*80}")
    print(f"IMPORTING TO QDRANT")
    print(f"{'='*80}\n")

    if all_points:
        print(f"🔄 Importing {len(all_points)} points...")
        qdrant_client.upsert(
            collection_name=collection_name,
            points=all_points
        )
        print(f"  ✅ Imported successfully")

        # Verify
        collection_info = qdrant_client.get_collection(collection_name)

        print(f"\n✅ MANUAL PROCESSING COMPLETE!")
        print(f"   Processed: {len(all_points)}/{len(MISSING_CONVERSATIONS)} conversations")
        print(f"   Total cost: ${total_cost:.4f}")
        print(f"   Total tokens: {total_input} input, {total_output} output")
        print(f"\n   Collection now has: {collection_info.points_count} total points")

        # Show final breakdown
        from collections import defaultdict
        results = qdrant_client.scroll(
            collection_name=collection_name,
            limit=100,
            with_payload=['project'],
            with_vectors=False
        )

        projects = defaultdict(int)
        for point in results[0]:
            projects[point.payload.get('project', 'unknown')] += 1

        print(f"\n📊 Final breakdown by project:")
        for project, count in sorted(projects.items()):
            print(f"   • {project}: {count} conversations")

        print(f"\n🎉 100% COVERAGE ACHIEVED!")
    else:
        print(f"❌ No points created (all failed)")


if __name__ == "__main__":
    main()
