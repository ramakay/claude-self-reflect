#!/usr/bin/env python3
"""
Test same conversation with different max_tokens to see quality difference.
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

Analyze FULL conversation for metadata FIRST, then write comprehensive narrative."""

def test_token_limit(client, conversation_text, max_tokens, label):
    """Test with specific max_tokens."""
    print(f"\n{'='*70}")
    print(f"{label}: max_tokens={max_tokens}")
    print(f"{'='*70}")

    response = client.messages.create(
        model="claude-sonnet-4-20250514",  # Sonnet 4.5
        max_tokens=max_tokens,
        system=SYSTEM_PROMPT,
        tools=[NARRATIVE_TOOL],
        tool_choice={"type": "tool", "name": "store_narrative"},
        messages=[{"role": "user", "content": f"Analyze this conversation:\n\n{conversation_text[:50000]}"}]
    )

    for content in response.content:
        if content.type == "tool_use" and content.name == "store_narrative":
            data = content.input

            narrative_len = len(data['narrative'])
            search_len = len(data['search_index'])
            tools = len(data['metadata']['tools_used'])
            concepts = len(data['metadata']['concepts'])
            files = len(data['metadata']['files_modified'])

            print(f"\n📊 Output Stats:")
            print(f"   Narrative length: {narrative_len:,} chars")
            print(f"   Search index length: {search_len:,} chars")
            print(f"   Tools extracted: {tools}/10")
            print(f"   Concepts extracted: {concepts}/10")
            print(f"   Files extracted: {files}/10")
            print(f"   Input tokens: {response.usage.input_tokens:,}")
            print(f"   Output tokens: {response.usage.output_tokens:,}")

            print(f"\n📝 Narrative preview (first 500 chars):")
            print("-" * 70)
            print(data['narrative'][:500])
            print("-" * 70)

            return {
                'max_tokens': max_tokens,
                'narrative_len': narrative_len,
                'search_len': search_len,
                'tools': tools,
                'concepts': concepts,
                'files': files,
                'output_tokens': response.usage.output_tokens,
                'data': data
            }

def main():
    print("=" * 70)
    print("TOKEN LIMIT COMPARISON TEST")
    print("=" * 70)
    print("Testing same conversation with different max_tokens")
    print()

    # Initialize
    anthropic_client = anthropic.Anthropic(api_key=os.getenv('ANTHROPIC_API_KEY'))
    qdrant_client = QdrantClient(url='http://localhost:6333')

    # Get a large conversation
    print("📊 Loading large conversation (conv_f87a171a_local - 514 chunks)...")
    results = qdrant_client.scroll(
        collection_name='conv_f87a171a_local',
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

    # Test different token limits
    results = []

    results.append(test_token_limit(
        anthropic_client,
        conversation_text,
        8192,
        "Test 1: 8K tokens (current)"
    ))

    results.append(test_token_limit(
        anthropic_client,
        conversation_text,
        16384,
        "Test 2: 16K tokens (2x)"
    ))

    results.append(test_token_limit(
        anthropic_client,
        conversation_text,
        32768,
        "Test 3: 32K tokens (4x)"
    ))

    # Summary comparison
    print("\n" + "=" * 70)
    print("COMPARISON SUMMARY")
    print("=" * 70)

    print(f"\n{'Max Tokens':<15} {'Output':<10} {'Narrative':<12} {'Tools':<8} {'Concepts':<10} {'Files':<8}")
    print("-" * 70)

    for r in results:
        print(f"{r['max_tokens']:<15} {r['output_tokens']:<10} {r['narrative_len']:<12} {r['tools']:<8} {r['concepts']:<10} {r['files']:<8}")

    print("\n🎯 ANALYSIS:")

    # Check if more tokens = more detail
    if results[2]['narrative_len'] > results[0]['narrative_len'] * 1.5:
        print("   ✅ Higher token limits produce SIGNIFICANTLY more detail")
        print(f"   📈 32K is {results[2]['narrative_len'] / results[0]['narrative_len']:.1f}x longer than 8K")
    else:
        print("   ⚠️  Higher token limits don't add much more detail")
        print(f"   📊 32K is only {results[2]['narrative_len'] / results[0]['narrative_len']:.1f}x longer than 8K")

    # Check metadata saturation
    if results[0]['tools'] == results[2]['tools'] == 10:
        print("   ⚠️  Metadata hitting limits (10/10 tools in both)")
    else:
        print(f"   📊 Metadata capture: 8K={results[0]['tools']}tools, 32K={results[2]['tools']}tools")

    print("\n💡 RECOMMENDATION:")
    ratio = results[2]['narrative_len'] / results[0]['narrative_len']
    if ratio > 2.0:
        print(f"   Use 32K+ tokens for {ratio:.1f}x more comprehensive narratives")
    elif ratio > 1.5:
        print(f"   Use 16K tokens for {ratio:.1f}x better detail (good balance)")
    else:
        print(f"   8K tokens is sufficient (only {ratio:.1f}x difference)")

if __name__ == '__main__':
    main()
