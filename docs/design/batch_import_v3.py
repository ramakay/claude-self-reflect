#!/usr/bin/env python3
"""Batch import conversations with V3+SKILL_V2 to Qdrant for comparison testing."""

import os
import sys
import json
from pathlib import Path
from dotenv import load_dotenv
import time

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


def process_conversation(jsonl_path: Path, client: anthropic.Anthropic, skill_instructions: str,
                         qdrant_client: QdrantClient, collection_name: str, embedding_model):
    """Process single conversation with V3+SKILL_V2 and import to Qdrant."""

    conv_id = jsonl_path.stem
    print(f"\n{'='*80}")
    print(f"Processing: {conv_id}")
    print(f"File: {jsonl_path.name}")
    print(f"{'='*80}")

    # Read messages
    messages = []
    with open(jsonl_path) as f:
        for line in f:
            if line.strip():
                messages.append(json.loads(line))

    print(f"Original messages: {len(messages)}")

    # V3 extraction
    print("\n🔄 Step 1: V3 Extraction...")
    result = extract_events_v3(messages)

    print(f"  Search index: {result['stats']['search_index_tokens']} tokens")
    print(f"  Context cache: {result['stats']['context_cache_tokens']} tokens")
    print(f"  Total: {result['stats']['total_tokens']} tokens")
    print(f"  Signature: {json.dumps(result['signature'], indent=2)}")

    # Generate narrative with Skill
    print("\n🔄 Step 2: Generating narrative with SKILL_V2...")

    prompt = f"""You are analyzing a development conversation. Use the SKILL_V2 guidelines to generate a search-optimized narrative.

## Extracted Events

### Search Index
{result['search_index']}

### Context Cache
{result['context_cache']}

### Conversation Signature
```json
{json.dumps(result['signature'], indent=2)}
```

Now generate the narrative following SKILL_V2 format exactly."""

    response = client.messages.create(
        model="claude-sonnet-4-5-20250929",
        max_tokens=2048,
        system=skill_instructions,
        messages=[{"role": "user", "content": prompt}]
    )

    # Extract narrative
    narrative = ""
    for block in response.content:
        if hasattr(block, 'text'):
            narrative += block.text

    # Calculate cost
    input_tokens = response.usage.input_tokens
    output_tokens = response.usage.output_tokens
    cost = (input_tokens * 3 + output_tokens * 15) / 1_000_000

    print(f"  Tokens: {input_tokens} input, {output_tokens} output")
    print(f"  Cost: ${cost:.6f}")

    # Generate embedding for the narrative
    print("\n🔄 Step 3: Generating embedding...")
    embedding = get_embedding(narrative, embedding_model)
    print(f"  Embedding dimensions: {len(embedding)}")

    # Import to Qdrant
    print("\n🔄 Step 4: Importing to Qdrant...")

    point = PointStruct(
        id=conv_id,
        vector=embedding,
        payload={
            "conversation_id": conv_id,
            "project": "claude-self-reflect",
            "narrative": narrative,
            "search_index": result['search_index'],
            "context_cache": result['context_cache'],
            "signature": result['signature'],
            "timestamp": time.time(),
            "extraction_stats": result['stats']
        }
    )

    qdrant_client.upsert(
        collection_name=collection_name,
        points=[point]
    )

    print(f"  ✅ Imported to collection: {collection_name}")

    return {
        'conversation_id': conv_id,
        'narrative': narrative,
        'stats': result['stats'],
        'cost': cost,
        'tokens': {'input': input_tokens, 'output': output_tokens}
    }


def main():
    """Main batch import process."""

    # Setup - use claude-self-reflect project
    project_dir = Path.home() / ".claude/projects/-Users-username-projects-claude-self-reflect"
    skill_v2_path = Path(__file__).parent / "conversation-analyzer" / "SKILL_V2.md"

    if not project_dir.exists():
        print(f"❌ Project directory not found: {project_dir}")
        sys.exit(1)

    if not skill_v2_path.exists():
        print(f"❌ SKILL_V2.md not found: {skill_v2_path}")
        sys.exit(1)

    # Find all conversations
    conversations = list(project_dir.glob("*.jsonl"))
    print(f"\n📊 Found {len(conversations)} conversations in claude-self-reflect")

    # Budget check
    estimated_cost = len(conversations) * 0.016  # Conservative estimate
    print(f"💰 Estimated cost: ${estimated_cost:.2f} (budget: $5.00)")

    if estimated_cost > 5.0:
        print(f"⚠️  Estimated cost exceeds budget!")
        limit = int(5.0 / 0.016)
        print(f"   Limiting to first {limit} conversations")
        conversations = conversations[:limit]

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

    # Load Skill instructions
    with open(skill_v2_path) as f:
        skill_instructions = f.read()

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

    # Create test collection
    collection_name = "v3_test_csr"
    print(f"\n🔧 Creating test collection: {collection_name}")

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

    # Process all conversations
    results = []
    total_cost = 0.0

    for i, conv_path in enumerate(conversations, 1):
        print(f"\n\n{'='*80}")
        print(f"CONVERSATION {i}/{len(conversations)}")
        print(f"{'='*80}")

        try:
            result = process_conversation(
                conv_path,
                anthropic_client,
                skill_instructions,
                qdrant_client,
                collection_name,
                embedding_model
            )
            results.append(result)
            total_cost += result['cost']

            print(f"\n✅ Success!")
            print(f"   Running cost: ${total_cost:.4f}")

        except Exception as e:
            print(f"\n❌ Error processing {conv_path.name}: {e}")
            import traceback
            traceback.print_exc()

    # Summary
    print(f"\n\n{'='*80}")
    print(f"BATCH IMPORT SUMMARY")
    print(f"{'='*80}")
    print(f"Total conversations processed: {len(results)}/{len(conversations)}")
    print(f"Total cost: ${total_cost:.4f}")
    print(f"Average cost per conversation: ${total_cost/len(results):.4f}")
    print(f"Collection: {collection_name}")
    print(f"\n🎯 Ready for comparison testing!")
    print(f"\nNext steps:")
    print(f"1. Search old collection: reflect_on_past(query, project='procsolve-website')")
    print(f"2. Search new collection: qdrant_client.search(collection_name='{collection_name}')")
    print(f"3. Compare results side-by-side")

    return results


if __name__ == "__main__":
    results = main()
