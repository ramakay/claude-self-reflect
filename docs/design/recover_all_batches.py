#!/usr/bin/env python3
"""
Recover ALL batch results from dashboard and complete Qdrant import.

Retrieves narratives from all 8 completed batches shown in dashboard.
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


# ALL Batch IDs from dashboard (complete list)
ALL_BATCHES = [
    'msgbatch_012GH6kVL74ihT3NFFHbrYHZ',  # 1 request (mystery - thegatehouse?)
    'msgbatch_01DMoYp2egP7Wz2Xa8Lv7cNc',  # 1 request (address-book-fix run 2)
    'msgbatch_01Prq1G5CbfjjDdyGezKUGzH',  # 5 requests (anukruti run 2)
    'msgbatch_01ATPhpjCw1gqPisHUgoPnab',  # 1 request (address-book-fix)
    'msgbatch_016g8zHtH7or7DtJu3ZzAczS',  # 5 requests (anukruti)
    'msgbatch_01QCwhFw9DYDJ8uPjYsHg8Xu',  # 5 requests (buyindian)
    'msgbatch_01WVbb5X2xYwuzzgEdqVicZJ',  # 2 requests (procsolve-website or cc-enhance)
    'msgbatch_01EemyvChmnShYAuJix7m1As',  # 36 requests (claude-self-reflect)
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


def load_conversation_data(projects_dir: Path):
    """Load V3 extraction results and metadata for ALL projects."""

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

    # Scan ALL project directories
    for project_dir in projects_dir.iterdir():
        if not project_dir.is_dir() or project_dir.name.startswith('.'):
            continue

        jsonl_files = list(project_dir.glob("*.jsonl"))
        if not jsonl_files:
            continue

        # Extract project name
        parts = project_dir.name.split('-projects-')
        project_name = parts[-1] if len(parts) > 1 else project_dir.name

        print(f"\n📂 Loading {project_name}...")

        for jsonl_file in jsonl_files:
            conv_id = jsonl_file.stem

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

            conversations[conv_id] = {
                'result': result,
                'project': project_name
            }

            print(f"  ✅ {conv_id[:8]}... ({project_name})")

    return conversations


def main():
    """Recover and import ALL batch results."""

    print(f"\n{'='*80}")
    print(f"COMPLETE BATCH RECOVERY & QDRANT IMPORT")
    print(f"{'='*80}\n")

    # Initialize clients
    print("🔧 Initializing clients...")
    anthropic_client = anthropic.Anthropic(api_key=os.getenv("ANTHROPIC_API_KEY"))
    qdrant_client = QdrantClient(url=os.getenv("QDRANT_URL", "http://localhost:6333"))
    embedding_model = TextEmbedding(model_name="sentence-transformers/all-MiniLM-L6-v2")
    print("  ✅ Clients initialized")

    # Collection name
    collection_name = "v3_all_projects"

    # Retrieve ALL batch results
    print(f"\n🔄 Retrieving {len(ALL_BATCHES)} batches...")
    all_narratives = {}
    grand_total_cost = 0.0

    for batch_id in ALL_BATCHES:
        narratives, cost = retrieve_batch_narratives(anthropic_client, batch_id)

        # Add narratives (with dedupe)
        for conv_id, narrative in narratives.items():
            if conv_id not in all_narratives:
                all_narratives[conv_id] = narrative
            else:
                print(f"  ⚠️  Duplicate {conv_id[:8]}... (skipping)")

        grand_total_cost += cost

    print(f"\n📊 Total unique narratives retrieved: {len(all_narratives)}")
    print(f"💰 Total cost: ${grand_total_cost:.4f}")

    # Load conversation data and create points
    print(f"\n🔄 Loading ALL conversation data...")

    projects_dir = Path.home() / ".claude/projects"
    conversations = load_conversation_data(projects_dir)

    print(f"\n✅ Loaded {len(conversations)} conversations from disk")

    # Match narratives to conversations and create points
    print(f"\n🔄 Creating points...")
    all_points = []

    for conv_id, conv_data in conversations.items():
        if conv_id not in all_narratives:
            print(f"  ⚠️  No narrative for {conv_id[:8]}... ({conv_data['project']})")
            continue

        narrative = all_narratives[conv_id]
        result = conv_data['result']
        project = conv_data['project']

        # Generate embedding
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
        print(f"  ✅ {conv_id[:8]}... ({project})")

    # Import to Qdrant (upsert to avoid duplicates)
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
    print(f"\n✅ COMPLETE RECOVERY DONE!")
    print(f"   Collection: {collection_name}")
    print(f"   Total points: {collection_info.points_count}")
    print(f"   Total cost: ${grand_total_cost:.4f}")

    # Show projects breakdown
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


if __name__ == "__main__":
    main()
