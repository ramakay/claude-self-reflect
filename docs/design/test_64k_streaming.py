#!/usr/bin/env python3
"""
Test 64K tokens with streaming to see if we capture EVERYTHING.
"""
import os
import sys
import json
from pathlib import Path
from dotenv import load_dotenv

load_dotenv()
sys.path.insert(0, str(Path(__file__).parent.parent.parent))

import anthropic
from qdrant_client import QdrantClient

NARRATIVE_TOOL = {
    "name": "store_narrative",
    "description": "Store the extracted narrative and metadata from a conversation",
    "input_schema": {
        "type": "object",
        "properties": {
            "narrative": {"type": "string"},
            "search_index": {"type": "string"},
            "metadata": {
                "type": "object",
                "properties": {
                    "tools_used": {"type": "array", "items": {"type": "string"}, "maxItems": 10},
                    "concepts": {"type": "array", "items": {"type": "string"}, "maxItems": 10},
                    "files_modified": {"type": "array", "items": {"type": "string"}, "maxItems": 10}
                },
                "required": ["tools_used", "concepts", "files_modified"]
            }
        },
        "required": ["narrative", "search_index", "metadata"]
    }
}

SYSTEM_PROMPT = """Extract comprehensive narrative from this Claude Code conversation.

Analyze FULL conversation for metadata FIRST, then write comprehensive narrative.

IMPORTANT: Include ALL major themes and work items, not just the main theme.
For example, if the conversation includes both a redesign AND OpenGraph work, 
include BOTH in the narrative."""

def main():
    print("=" * 70)
    print("64K STREAMING TEST")
    print("=" * 70)
    print("Testing if 64K tokens can capture EVERYTHING in one narrative")
    print()

    # Initialize clients
    anthropic_client = anthropic.Anthropic(api_key=os.getenv('ANTHROPIC_API_KEY'))
    qdrant_client = QdrantClient(url='http://localhost:6333')

    # Get the large procsolve conversation (1000 chunks)
    print("📊 Loading conv_9f2f312b_local (1000 chunks)...")
    results = qdrant_client.scroll(
        collection_name='conv_9f2f312b_local',
        limit=1000,
        with_payload=True
    )

    chunks = []
    for point in results[0]:
        chunk_text = point.payload.get('text', point.payload.get('content', ''))
        if chunk_text:
            chunks.append(chunk_text)

    conversation_text = "\n\n---CHUNK---\n\n".join(chunks)
    print(f"✅ Loaded {len(chunks)} chunks ({len(conversation_text):,} chars)")
    
    # Test with streaming + 64K
    print(f"\n{'='*70}")
    print(f"Testing: 64K tokens with streaming")
    print(f"{'='*70}")

    print("\n🔄 Streaming response...")
    
    collected_text = ""
    tool_use_data = None
    
    with anthropic_client.messages.stream(
        model="claude-sonnet-4-20250514",
        max_tokens=64000,  # Actual limit for Sonnet 4.5
        system=SYSTEM_PROMPT,
        tools=[NARRATIVE_TOOL],
        tool_choice={"type": "tool", "name": "store_narrative"},
        messages=[{
            "role": "user", 
            "content": f"Analyze this conversation:\n\n{conversation_text[:100000]}"  # More input too!
        }]
    ) as stream:
        for event in stream:
            if hasattr(event, 'type'):
                if event.type == 'content_block_delta':
                    if hasattr(event.delta, 'partial_json'):
                        collected_text += event.delta.partial_json
                elif event.type == 'content_block_stop':
                    # Parse the complete tool use
                    try:
                        tool_use_data = json.loads(collected_text)
                    except:
                        pass

    if tool_use_data:
        narrative = tool_use_data['narrative']
        search_index = tool_use_data['search_index']
        metadata = tool_use_data['metadata']
        
        print(f"\n✅ Streaming complete!")
        print(f"   Narrative length: {len(narrative):,} chars")
        print(f"   Search index length: {len(search_index):,} chars")
        print(f"   Tools: {len(metadata['tools_used'])}/10")
        print(f"   Concepts: {len(metadata['concepts'])}/10")
        print(f"   Files: {len(metadata['files_modified'])}/10")
        
        print(f"\n📄 FULL NARRATIVE:")
        print("=" * 70)
        print(narrative)
        print("=" * 70)
        
        # Check if OpenGraph is mentioned
        print(f"\n🔍 ANALYSIS:")
        if 'opengraph' in narrative.lower() or 'og:' in narrative.lower():
            print("   ✅ OpenGraph IS mentioned!")
        else:
            print("   ❌ OpenGraph NOT mentioned")
            
        if 'proc1' in narrative.lower() or 'redesign' in narrative.lower():
            print("   ✅ Proc1 redesign IS mentioned!")
        else:
            print("   ❌ Proc1 redesign NOT mentioned")
            
        # Compare to 16K version
        print(f"\n📊 COMPARISON TO 16K:")
        print(f"   16K narrative: 4,258 chars")
        print(f"   64K narrative: {len(narrative):,} chars")
        print(f"   Improvement: {len(narrative) / 4258:.1f}x longer")
        
    else:
        print("\n❌ Failed to parse tool use data")
        print(f"Collected text: {collected_text[:500]}")

if __name__ == '__main__':
    main()
