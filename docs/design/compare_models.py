#!/usr/bin/env python3
"""
Compare narrative generation across 3 models for the same conversation.
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

# Model configurations
MODELS = [
    {
        'name': 'Claude 3.5 Sonnet',
        'id': 'claude-3-5-sonnet-20241022',
        'max_tokens': 8192  # Model limit
    },
    {
        'name': 'Claude Sonnet 4.5',
        'id': 'claude-sonnet-4-20250514',
        'max_tokens': 16384  # Test with 16K (64K available!)
    },
    {
        'name': 'Claude Opus 4',
        'id': 'claude-opus-4-20250514',
        'max_tokens': 16384  # Model limit is 16K
    }
]

SYSTEM_PROMPT = """You are a conversation analyzer. Extract a comprehensive narrative from this Claude Code conversation.

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

def get_conversation_chunks(qdrant_client, collection_name: str) -> list:
    """Get all chunks from a conversation."""
    results = qdrant_client.scroll(
        collection_name=collection_name,
        limit=1000,
        with_payload=True
    )

    chunks = []
    for point in results[0]:
        chunk_text = point.payload.get('text', point.payload.get('content', ''))
        if chunk_text:
            chunks.append(chunk_text)

    return chunks

def test_model(client, model_config, conversation_text):
    """Test a single model."""
    print(f"\n{'='*70}")
    print(f"Testing: {model_config['name']}")
    print(f"Model ID: {model_config['id']}")
    print(f"Max tokens: {model_config['max_tokens']}")
    print(f"{'='*70}")

    user_prompt = f"""Analyze this conversation and create a comprehensive narrative:

CONVERSATION:
{conversation_text[:50000]}

Generate the narrative following the system instructions."""

    start_time = time.time()

    try:
        response = client.messages.create(
            model=model_config['id'],
            max_tokens=model_config['max_tokens'],
            system=SYSTEM_PROMPT,
            messages=[
                {
                    "role": "user",
                    "content": user_prompt
                }
            ]
        )

        elapsed = time.time() - start_time
        content = response.content[0].text

        print(f"\n✅ Success!")
        print(f"⏱️  Time: {elapsed:.2f}s")
        print(f"📊 Input tokens: {response.usage.input_tokens}")
        print(f"📊 Output tokens: {response.usage.output_tokens}")
        print(f"\n📄 Response preview (first 500 chars):")
        print("-" * 70)
        print(content[:500])
        print("-" * 70)

        # Try to parse as JSON
        try:
            # Handle markdown code fences
            json_content = content
            if '```json' in content:
                json_start = content.find('```json') + 7
                json_end = content.find('```', json_start)
                json_content = content[json_start:json_end].strip()
            elif '```' in content:
                json_start = content.find('```') + 3
                json_end = content.find('```', json_start)
                json_content = content[json_start:json_end].strip()

            # Try to fix backticks
            import re
            json_content = re.sub(
                r':\s*`([^`]*)`',
                lambda m: f': "{m.group(1).replace(chr(10), "\\n").replace('"', '\\"')}"',
                json_content,
                flags=re.DOTALL
            )

            parsed = json.loads(json_content)
            print(f"\n✅ JSON parsing: SUCCESS")
            print(f"   - Has narrative: {'narrative' in parsed}")
            print(f"   - Has search_index: {'search_index' in parsed}")
            print(f"   - Has metadata: {'metadata' in parsed}")

            if 'metadata' in parsed:
                meta = parsed['metadata']
                print(f"   - Tools: {len(meta.get('tools_used', []))}")
                print(f"   - Concepts: {len(meta.get('concepts', []))}")
                print(f"   - Files: {len(meta.get('files_modified', []))}")

        except Exception as e:
            print(f"\n❌ JSON parsing: FAILED")
            print(f"   Error: {str(e)[:100]}")

        return {
            'success': True,
            'time': elapsed,
            'tokens': response.usage.output_tokens,
            'content': content,
            'parsable': 'parsed' in locals()
        }

    except Exception as e:
        elapsed = time.time() - start_time
        print(f"\n❌ Failed!")
        print(f"⏱️  Time: {elapsed:.2f}s")
        print(f"❌ Error: {str(e)[:200]}")

        return {
            'success': False,
            'time': elapsed,
            'error': str(e)
        }

def main():
    print("=" * 70)
    print("MODEL COMPARISON TEST")
    print("=" * 70)
    print("Testing 1 conversation with 3 models")
    print()

    # Initialize clients
    anthropic_client = anthropic.Anthropic(api_key=os.getenv('ANTHROPIC_API_KEY'))
    qdrant_client = QdrantClient(url='http://localhost:6333')

    # Get a failed conversation (tier1_002 -> conv_f87a171a_local)
    print("📊 Loading conversation from conv_f87a171a_local...")
    chunks = get_conversation_chunks(qdrant_client, 'conv_f87a171a_local')
    conversation_text = "\n\n---CHUNK---\n\n".join(chunks)

    print(f"✅ Loaded {len(chunks)} chunks ({len(conversation_text)} chars)")

    # Test all models
    results = {}
    for model in MODELS:
        results[model['name']] = test_model(anthropic_client, model, conversation_text)
        time.sleep(2)  # Rate limiting

    # Summary
    print("\n" + "=" * 70)
    print("COMPARISON SUMMARY")
    print("=" * 70)

    print(f"\n{'Model':<25} {'Success':<10} {'Time':<10} {'Tokens':<10} {'Parsable':<10}")
    print("-" * 70)

    for model_name, result in results.items():
        success = "✅" if result['success'] else "❌"
        time_str = f"{result.get('time', 0):.2f}s"
        tokens = result.get('tokens', '-')
        parsable = "✅" if result.get('parsable') else "❌"
        print(f"{model_name:<25} {success:<10} {time_str:<10} {tokens:<10} {parsable:<10}")

    print("\n🏆 WINNER:")
    successful = [(name, r) for name, r in results.items() if r['success'] and r.get('parsable')]
    if successful:
        best = min(successful, key=lambda x: x[1]['time'])
        print(f"   {best[0]} - Fastest parsable response ({best[1]['time']:.2f}s)")
    else:
        print("   No fully successful model!")


if __name__ == '__main__':
    main()
