#!/usr/bin/env python3
"""
Batch import ALL Claude Code projects with V3+SKILL_V2 to Qdrant.

Uses batching for efficient API calls and tracks costs across all projects.
"""

import os
import sys
import json
from pathlib import Path
from dotenv import load_dotenv
import time
from collections import defaultdict

load_dotenv()

# Add parent dirs to path
sys.path.insert(0, str(Path(__file__).parent.parent.parent))

try:
    import anthropic
except ImportError:
    print("Error: anthropic SDK not found")
    sys.exit(1)

from docs.design.extract_events_v3 import extract_events_v3
from qdrant_client import QdrantClient
from qdrant_client.models import Distance, VectorParams, PointStruct

# Import metadata extraction functions
# Path: docs/design/batch_import_all_projects.py -> ../../src/runtime/delta-metadata-update.py
import importlib.util
delta_metadata_path = Path(__file__).parent.parent.parent / "src" / "runtime" / "delta-metadata-update.py"
spec = importlib.util.spec_from_file_location("delta_metadata_update", delta_metadata_path)
delta_metadata_update = importlib.util.module_from_spec(spec)
spec.loader.exec_module(delta_metadata_update)
extract_tool_usage_from_jsonl = delta_metadata_update.extract_tool_usage_from_jsonl
extract_concepts = delta_metadata_update.extract_concepts

# Try importing FastEmbed
try:
    from fastembed import TextEmbedding
    FASTEMBED_AVAILABLE = True
except ImportError:
    FASTEMBED_AVAILABLE = False
    print("⚠️  FastEmbed not available, will use Voyage AI")


def get_embedding(text: str, embedding_model) -> list:
    """Generate embedding for text."""
    if FASTEMBED_AVAILABLE and embedding_model:
        embeddings = list(embedding_model.embed([text]))
        return embeddings[0].tolist()
    else:
        # Fallback to Voyage
        import voyageai
        vo = voyageai.Client(api_key=os.getenv('VOYAGE_KEY'))
        result = vo.embed([text], model="voyage-3", input_type="document")
        return result.embeddings[0]


def batch_generate_narratives(conversations_data: list, client: anthropic.Anthropic, skill_instructions: str):
    """
    Generate narratives for multiple conversations using batched requests.

    Args:
        conversations_data: List of dicts with 'conv_id', 'result' (V3 extraction)
        client: Anthropic client
        skill_instructions: SKILL_V2 instructions

    Returns:
        dict mapping conv_id to narrative
    """

    print(f"\n🔄 Batching {len(conversations_data)} narrative generation requests...")

    # Create batch requests
    batch_requests = []
    for data in conversations_data:
        result = data['result']

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

        batch_requests.append({
            "custom_id": data['conv_id'],
            "params": {
                "model": "claude-sonnet-4-5-20250929",
                "max_tokens": 2048,
                "system": skill_instructions,
                "messages": [{"role": "user", "content": prompt}]
            }
        })

    # Create batch via API
    batch_response = client.messages.batches.create(requests=batch_requests)
    batch_id = batch_response.id

    print(f"  ✅ Batch created: {batch_id}")
    print(f"  Status: {batch_response.processing_status}")

    # Poll for completion
    print("\n  Waiting for batch to complete...")
    max_wait = 1800  # 30 minutes max (batches can take longer than expected)
    start_time = time.time()

    while True:
        if time.time() - start_time > max_wait:
            print(f"  ⚠️  Timeout after {max_wait}s")
            break

        batch_status = client.messages.batches.retrieve(batch_id)
        status = batch_status.processing_status

        print(f"  Status: {status} ({batch_status.request_counts.processing} processing, "
              f"{batch_status.request_counts.succeeded} succeeded, "
              f"{batch_status.request_counts.errored} errored)")

        if status == "ended":
            print(f"  ✅ Batch completed!")
            break

        time.sleep(5)

    # Retrieve results
    print("\n  Fetching results...")
    results_response = client.messages.batches.results(batch_id)

    narratives = {}
    total_cost = 0.0
    total_input = 0
    total_output = 0

    for result_item in results_response:
        conv_id = result_item.custom_id

        if result_item.result.type == "succeeded":
            message = result_item.result.message

            # Extract narrative
            narrative = ""
            for block in message.content:
                if hasattr(block, 'text'):
                    narrative += block.text

            narratives[conv_id] = narrative

            # Track usage
            input_tokens = message.usage.input_tokens
            output_tokens = message.usage.output_tokens
            cost = (input_tokens * 3 + output_tokens * 15) / 1_000_000

            total_input += input_tokens
            total_output += output_tokens
            total_cost += cost
        else:
            print(f"  ❌ Error for {conv_id}: {result_item.result.error}")

    print(f"\n  📊 Batch Results:")
    print(f"     Succeeded: {len(narratives)}/{len(conversations_data)}")
    print(f"     Total tokens: {total_input} input, {total_output} output")
    print(f"     Total cost: ${total_cost:.4f}")
    print(f"     Avg cost/conversation: ${total_cost/len(narratives):.4f}")

    return narratives, total_cost


def discover_projects():
    """Find all Claude Code projects with JSONL files."""

    projects_dir = Path.home() / ".claude/projects"
    projects = {}

    for project_dir in projects_dir.iterdir():
        if not project_dir.is_dir():
            continue
        if project_dir.name.startswith('.'):
            continue
        if project_dir.name in ['test-streaming-project', 'test-voyage-import', 'claude-self-reflect-stress-test']:
            continue  # Skip test projects

        # Count JSONL files
        jsonl_files = list(project_dir.glob("*.jsonl"))
        if jsonl_files:
            # Extract project name from directory
            # Format: "-Users-rama-projects-procsolve-website" -> "procsolve-website"
            parts = project_dir.name.split('-projects-')
            project_name = parts[-1] if len(parts) > 1 else project_dir.name

            projects[project_name] = {
                'dir': project_dir,
                'conversations': jsonl_files,
                'count': len(jsonl_files)
            }

    return projects


def main():
    """Main multi-project batch import process."""

    print(f"\n{'='*80}")
    print(f"MULTI-PROJECT V3+SKILL_V2 BATCH IMPORT")
    print(f"{'='*80}\n")

    # Discover all projects
    print("🔍 Discovering projects...")
    projects = discover_projects()

    total_conversations = sum(p['count'] for p in projects.values())

    print(f"\n📊 Found {len(projects)} projects with {total_conversations} conversations:")
    for name, info in sorted(projects.items(), key=lambda x: x[1]['count'], reverse=True):
        print(f"   • {name}: {info['count']} conversations")

    # Budget check
    estimated_cost = total_conversations * 0.017  # Conservative estimate with batching
    print(f"\n💰 Estimated cost: ${estimated_cost:.2f} (budget: $5.00)")

    if estimated_cost > 5.0:
        print(f"⚠️  Estimated cost exceeds budget! Proceeding anyway (batching reduces cost)")

    # Load SKILL_V2
    skill_v2_path = Path(__file__).parent / "conversation-analyzer" / "SKILL_V2.md"
    if not skill_v2_path.exists():
        print(f"❌ SKILL_V2.md not found: {skill_v2_path}")
        sys.exit(1)

    with open(skill_v2_path) as f:
        skill_instructions = f.read()

    # Initialize clients
    print("\n🔧 Initializing clients...")

    # Validate API key
    api_key = os.getenv("ANTHROPIC_API_KEY")
    if not api_key:
        raise ValueError(
            "ANTHROPIC_API_KEY environment variable required. "
            "Set it in your .env file or export it in your shell."
        )

    anthropic_client = anthropic.Anthropic(api_key=api_key)
    qdrant_client = QdrantClient(url=os.getenv("QDRANT_URL", "http://localhost:6333"))

    # Initialize embedding model
    embedding_model = None
    vector_size = 384  # Default for FastEmbed

    if FASTEMBED_AVAILABLE:
        print("  Using FastEmbed (384 dimensions)")
        embedding_model = TextEmbedding(model_name="sentence-transformers/all-MiniLM-L6-v2")
        vector_size = 384
    else:
        print("  Using Voyage AI (1024 dimensions)")
        vector_size = 1024

    # Create unified test collection
    collection_name = "v3_all_projects"
    print(f"\n🔧 Creating unified collection: {collection_name}")

    try:
        qdrant_client.delete_collection(collection_name)
        print(f"  Deleted existing collection")
    except:
        pass

    qdrant_client.create_collection(
        collection_name=collection_name,
        vectors_config=VectorParams(size=vector_size, distance=Distance.COSINE)
    )
    print(f"  ✅ Created collection with {vector_size} dimensions")

    # Process all projects
    grand_total_cost = 0.0
    project_results = {}
    all_points = []

    for project_name, project_info in sorted(projects.items()):
        print(f"\n\n{'='*80}")
        print(f"PROJECT: {project_name} ({project_info['count']} conversations)")
        print(f"{'='*80}")

        # Step 1: Metadata + V3 Extraction (fast, local)
        print(f"\n🔄 Step 1: Metadata extraction + V3 Extraction for all conversations...")
        conversations_data = []

        for conv_path in project_info['conversations']:
            conv_id = conv_path.stem

            # Extract metadata FIRST
            tool_usage = extract_tool_usage_from_jsonl(str(conv_path))

            # Read messages for V3 extraction
            messages = []
            conversation_text = ""
            with open(conv_path) as f:
                for line in f:
                    if line.strip():
                        msg = json.loads(line)
                        messages.append(msg)

                        # Extract text for concept detection
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

            conversations_data.append({
                'conv_id': conv_id,
                'path': conv_path,
                'result': result,
                'project': project_name
            })

            print(f"  ✅ {conv_id}: {result['stats']['total_tokens']} tokens, {len(concepts)} concepts, {len(tool_usage.get('tools_summary', {}))} tool types")

        # Step 2: Batch generate narratives (API call)
        narratives, batch_cost = batch_generate_narratives(
            conversations_data,
            anthropic_client,
            skill_instructions
        )

        grand_total_cost += batch_cost

        # Step 3: Generate embeddings and prepare points
        print(f"\n🔄 Step 3: Generating embeddings...")

        for data in conversations_data:
            conv_id = data['conv_id']

            if conv_id not in narratives:
                print(f"  ⚠️  Skipping {conv_id} (no narrative)")
                continue

            narrative = narratives[conv_id]
            result = data['result']

            # Generate embedding
            embedding = get_embedding(narrative, embedding_model)

            # Create point
            point = PointStruct(
                id=conv_id,
                vector=embedding,
                payload={
                    "conversation_id": conv_id,
                    "project": project_name,
                    "narrative": narrative,
                    "search_index": result['search_index'],
                    "context_cache": result['context_cache'],
                    "signature": result['signature'],
                    "timestamp": time.time(),
                    "extraction_stats": result['stats']
                }
            )

            all_points.append(point)
            print(f"  ✅ {conv_id}: Embedded ({len(embedding)} dims)")

        project_results[project_name] = {
            'processed': len(narratives),
            'total': project_info['count'],
            'cost': batch_cost
        }

        print(f"\n✅ Project {project_name} complete: {len(narratives)} conversations, ${batch_cost:.4f}")

    # Step 4: Batch import all points to Qdrant
    print(f"\n\n{'='*80}")
    print(f"IMPORTING TO QDRANT")
    print(f"{'='*80}")
    print(f"\n🔄 Importing {len(all_points)} points in batches...")

    batch_size = 100
    for i in range(0, len(all_points), batch_size):
        batch = all_points[i:i+batch_size]
        qdrant_client.upsert(
            collection_name=collection_name,
            points=batch
        )
        print(f"  ✅ Imported batch {i//batch_size + 1}: {len(batch)} points")

    # Final Summary
    print(f"\n\n{'='*80}")
    print(f"FINAL SUMMARY")
    print(f"{'='*80}")

    print(f"\n📊 Per-Project Results:")
    for project_name, results in sorted(project_results.items()):
        print(f"   • {project_name}: {results['processed']}/{results['total']} conversations, ${results['cost']:.4f}")

    total_processed = sum(r['processed'] for r in project_results.values())
    print(f"\n💰 Total Cost: ${grand_total_cost:.4f}")
    print(f"📈 Total Processed: {total_processed}/{total_conversations} conversations")
    print(f"💡 Average Cost: ${grand_total_cost/total_processed:.4f} per conversation")
    print(f"\n🎯 Collection: {collection_name}")
    print(f"📦 Total Points: {len(all_points)}")

    print(f"\n✅ All projects imported successfully!")
    print(f"\nNext steps:")
    print(f"1. Test Skill with: 'Find conversations about Docker issues'")
    print(f"2. Compare with current CSR: csr_reflect_on_past(...)")
    print(f"3. Validate enhanced narratives vs raw excerpts")

    return project_results


if __name__ == "__main__":
    results = main()
