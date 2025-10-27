#!/usr/bin/env python3
"""
Recover batch results and complete the Qdrant import.

Retrieves narratives from completed batches and imports to Qdrant.
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


# Batch IDs from dashboard (in chronological order)
COMPLETED_BATCHES = [
    {
        'batch_id': 'msgbatch_01ATPhpjCw1gqPisHUgoPnab',
        'project': 'address-book-fix',
        'count': 1
    },
    {
        'batch_id': 'msgbatch_016g8zHtH7or7DtJu3ZzAczS',
        'project': 'anukruti',
        'count': 5
    },
    {
        'batch_id': 'msgbatch_01QCwhFw9DYDJ8uPjYsHg8Xu',
        'project': 'buyindian',
        'count': 5
    },
    {
        'batch_id': 'msgbatch_01WVbb5X2xYwuzzgEdqVicZJ',
        'project': 'procsolve-website',  # or cc-enhance
        'count': 2
    },
    {
        'batch_id': 'msgbatch_01EemyvChmnShYAuJix7m1As',
        'project': 'claude-self-reflect',
        'count': 36
    }
]


def get_embedding(text: str, embedding_model) -> list:
    """Generate embedding for text."""
    embeddings = list(embedding_model.embed([text]))
    return embeddings[0].tolist()


def retrieve_batch_narratives(client: anthropic.Anthropic, batch_id: str):
    """Retrieve narratives from a completed batch."""

    print(f"\n🔄 Retrieving batch {batch_id}...")

    try:
        # Get batch results
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

        print(f"  ✅ Retrieved {len(narratives)} narratives")
        print(f"  📊 Tokens: {total_input} input, {total_output} output")
        print(f"  💰 Cost: ${total_cost:.4f}")

        return narratives, total_cost

    except Exception as e:
        print(f"  ❌ Failed to retrieve batch: {e}")
        return {}, 0.0


def load_conversation_data(project_dir: Path):
    """Load V3 extraction results and metadata for a project."""

    conversations = {}

    # Import metadata extraction functions
    import importlib.util
    delta_metadata_path = Path(__file__).parent.parent.parent / "src" / "runtime" / "delta-metadata-update.py"
    spec = importlib.util.spec_from_file_location("delta_metadata_update", delta_metadata_path)
    delta_metadata_update = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(delta_metadata_update)
    extract_tool_usage_from_jsonl = delta_metadata_update.extract_tool_usage_from_jsonl
    extract_concepts = delta_metadata_update.extract_concepts

    from docs.design.extract_events_v3 import extract_events_v3

    for jsonl_file in project_dir.glob("*.jsonl"):
        conv_id = jsonl_file.stem

        # Extract metadata
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

        conversations[conv_id] = result

    return conversations


def main():
    """Recover and import batch results."""

    print(f"\n{'='*80}")
    print(f"BATCH RESULTS RECOVERY & QDRANT IMPORT")
    print(f"{'='*80}\n")

    # Initialize clients
    print("🔧 Initializing clients...")
    anthropic_client = anthropic.Anthropic(api_key=os.getenv("ANTHROPIC_API_KEY"))
    qdrant_client = QdrantClient(url=os.getenv("QDRANT_URL", "http://localhost:6333"))
    embedding_model = TextEmbedding(model_name="sentence-transformers/all-MiniLM-L6-v2")
    print("  ✅ Clients initialized")

    # Collection should already exist
    collection_name = "v3_all_projects"

    # Retrieve all batch results
    all_narratives = {}
    grand_total_cost = 0.0

    for batch_info in COMPLETED_BATCHES:
        narratives, cost = retrieve_batch_narratives(
            anthropic_client,
            batch_info['batch_id']
        )

        # Tag narratives with project
        for conv_id, narrative in narratives.items():
            all_narratives[conv_id] = {
                'narrative': narrative,
                'project': batch_info['project']
            }

        grand_total_cost += cost

    print(f"\n📊 Total narratives retrieved: {len(all_narratives)}")
    print(f"💰 Total cost: ${grand_total_cost:.4f}")

    # Load conversation data and create points
    print(f"\n🔄 Loading conversation data and creating points...")

    projects_dir = Path.home() / ".claude/projects"
    all_points = []

    for batch_info in COMPLETED_BATCHES:
        project_name = batch_info['project']

        # Find project directory
        project_dirs = list(projects_dir.glob(f"*{project_name}"))
        if not project_dirs:
            print(f"  ⚠️  Project directory not found for {project_name}")
            continue

        project_dir = project_dirs[0]
        print(f"\n  Processing {project_name}...")

        # Load conversation data
        conversations = load_conversation_data(project_dir)

        # Create points
        for conv_id, result in conversations.items():
            if conv_id not in all_narratives:
                print(f"    ⚠️  No narrative for {conv_id}")
                continue

            narrative = all_narratives[conv_id]['narrative']

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
            print(f"    ✅ {conv_id}")

    # Import to Qdrant
    print(f"\n🔄 Importing {len(all_points)} points to Qdrant...")

    batch_size = 100
    for i in range(0, len(all_points), batch_size):
        batch = all_points[i:i+batch_size]
        qdrant_client.upsert(
            collection_name=collection_name,
            points=batch
        )
        print(f"  ✅ Imported batch {i//batch_size + 1}: {len(batch)} points")

    # Verify
    collection_info = qdrant_client.get_collection(collection_name)
    print(f"\n✅ RECOVERY COMPLETE!")
    print(f"   Collection: {collection_name}")
    print(f"   Total points: {collection_info.points_count}")
    print(f"   Total cost: ${grand_total_cost:.4f}")


if __name__ == "__main__":
    main()
