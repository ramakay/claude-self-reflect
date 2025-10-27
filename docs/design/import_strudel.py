#!/usr/bin/env python3
"""Import strudel project conversations with V3+SKILL_V2 narratives."""

import os
import sys
from pathlib import Path
from dotenv import load_dotenv

load_dotenv()

# Add parent dirs to path
sys.path.insert(0, str(Path(__file__).parent.parent.parent))

from docs.design.batch_import_all_projects import (
    extract_events_v3,
    QdrantClient,
    PointStruct,
    json,
    time,
    anthropic,
    get_embedding,
    FASTEMBED_AVAILABLE,
    extract_tool_usage_from_jsonl,
    extract_concepts
)

try:
    from fastembed import TextEmbedding
except ImportError:
    TextEmbedding = None

# Configuration
STRUDEL_CONV_DIR = Path.home() / ".claude/projects/-Users-username-projects-strudel"
COLLECTION_NAME = "v3_all_projects"
QDRANT_URL = "http://localhost:6333"

def import_strudel():
    """Import strudel conversations with narratives."""

    # Initialize Qdrant
    qdrant = QdrantClient(url=QDRANT_URL)

    # Initialize embedding model
    if FASTEMBED_AVAILABLE:
        embedding_model = TextEmbedding(model_name="BAAI/bge-small-en-v1.5")
        print("✅ Using FastEmbed (384 dimensions)")
    else:
        embedding_model = None
        print("⚠️  Using Voyage AI")

    # Get strudel conversation files
    conv_files = list(STRUDEL_CONV_DIR.glob("*.jsonl"))
    print(f"\n📂 Found {len(conv_files)} strudel conversations")

    for conv_file in conv_files:
        conv_id = conv_file.stem
        print(f"\n🔄 Processing: {conv_id}")

        # Read conversation
        with open(conv_file, 'r') as f:
            messages = [json.loads(line) for line in f if line.strip()]

        print(f"   Messages: {len(messages)}")

        # Extract events with V3
        extracted = extract_events_v3(messages)
        print(f"   Events extracted: {len(extracted.get('events', []))}")

        # Extract metadata
        tools_used = extract_tool_usage_from_jsonl(str(conv_file))
        concepts = extract_concepts(messages, tools_used)

        # Create narrative using Batch API
        print(f"   Generating narrative via Batch API...")

        # Build narrative request
        user_request = "\n".join([
            msg.get('text', '') for msg in messages
            if msg.get('role') == 'user'
        ])[:1000]

        # Create batch request
        client = anthropic.Anthropic()

        # Load SKILL_V2 template
        skill_path = Path(__file__).parent / "conversation-analyzer" / "SKILL_V2.md"
        with open(skill_path, 'r') as f:
            skill_template = f.read()

        # Create prompt
        prompt = f"""Analyze this code session and create a narrative following the SKILL_V2 template.

Project: strudel
Conversation ID: {conv_id}
Messages: {len(messages)}

User Request:
{user_request}

Events Extracted:
{json.dumps(extracted, indent=2)[:2000]}

Tools Used: {', '.join(tools_used[:10])}
Concepts: {', '.join(concepts[:10])}

{skill_template}

Please create a complete narrative for this session."""

        # Submit single batch request
        batch_request = {
            "custom_id": conv_id,
            "params": {
                "model": "claude-haiku-4-5",
                "max_tokens": 4096,
                "messages": [{"role": "user", "content": prompt}]
            }
        }

        # Write batch file
        batch_file = Path(__file__).parent / f"strudel_{conv_id}_batch.jsonl"
        with open(batch_file, 'w') as f:
            f.write(json.dumps(batch_request) + '\n')

        # Submit batch
        with open(batch_file, 'r') as f:
            requests = [json.loads(line) for line in f if line.strip()]

        batch = client.messages.batches.create(requests=requests)
        print(f"   ✅ Batch submitted: {batch.id}")

        # Wait for completion (should be ~30 seconds with Haiku)
        print(f"   ⏳ Waiting for narrative generation...")
        while True:
            batch_status = client.messages.batches.retrieve(batch.id)
            if batch_status.processing_status == "ended":
                break
            time.sleep(5)

        print(f"   ✅ Batch completed")

        # Retrieve results
        results = list(client.messages.batches.results(batch.id))
        if results and results[0].result.type == "succeeded":
            narrative_text = results[0].result.message.content[0].text
            print(f"   ✅ Narrative generated ({len(narrative_text)} chars)")
        else:
            narrative_text = f"Failed to generate narrative for {conv_id}"
            print(f"   ❌ Narrative generation failed")

        # Create search index
        search_index = f"""Project: strudel
Conversation: {conv_id}
User Request: {user_request}
Tools: {', '.join(tools_used[:10])}
Concepts: {', '.join(concepts[:10])}
"""

        # Embed search index
        embedding = get_embedding(search_index, embedding_model)

        # Create point
        point = PointStruct(
            id=conv_id,
            vector=embedding,
            payload={
                "conversation_id": conv_id,
                "project": "strudel",
                "narrative": narrative_text,
                "search_index": search_index,
                "extracted_events": extracted,
                "tools_used": tools_used[:20],
                "concepts": concepts[:20],
                "message_count": len(messages),
                "context_cache": json.dumps(extracted)[:5000]
            }
        )

        # Upsert to Qdrant
        qdrant.upsert(
            collection_name=COLLECTION_NAME,
            points=[point]
        )

        print(f"   ✅ Imported to Qdrant")

        # Cleanup batch file
        batch_file.unlink()

    print(f"\n✅ Strudel import complete!")
    print(f"   {len(conv_files)} conversations imported with narratives")

if __name__ == "__main__":
    import_strudel()
