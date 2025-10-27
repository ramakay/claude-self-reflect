#!/usr/bin/env python3
"""Test V3 extraction + SKILL_V2 to see actual narrative quality."""

import os
import sys
from pathlib import Path
from dotenv import load_dotenv

load_dotenv()

try:
    import anthropic
except ImportError:
    print("Error: anthropic SDK not found")
    sys.exit(1)

from extract_events_v3 import extract_events_v3
import json


def test_v3_with_skill_v2(jsonl_path: Path):
    """Test the full pipeline: V3 extraction → SKILL_V2 → narrative."""

    client = anthropic.Anthropic(api_key=os.getenv("ANTHROPIC_API_KEY"))

    # Read messages
    messages = []
    with open(jsonl_path) as f:
        for line in f:
            if line.strip():
                messages.append(json.loads(line))

    # V3 extraction
    print("=" * 80)
    print("STEP 1: V3 EXTRACTION")
    print("=" * 80)
    result = extract_events_v3(messages)

    print(f"Original: {result['stats']['original_messages']} messages")
    print(f"Search index: {result['stats']['search_index_tokens']} tokens")
    print(f"Context cache: {result['stats']['context_cache_tokens']} tokens")
    print(f"Total: {result['stats']['total_tokens']} tokens")
    print(f"\nSignature: {json.dumps(result['signature'], indent=2)}")

    # Read SKILL_V2 instructions
    skill_v2_path = Path(__file__).parent / "conversation-analyzer" / "SKILL_V2.md"
    with open(skill_v2_path) as f:
        skill_instructions = f.read()

    # Build prompt for Sonnet 4.5
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

    # Call Claude Sonnet 4.5
    print("\n" + "=" * 80)
    print("STEP 2: GENERATING NARRATIVE WITH SONNET 4.5 + SKILL_V2")
    print("=" * 80)

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

    print(f"\nTokens: {input_tokens} input, {output_tokens} output")
    print(f"Cost: ${cost:.6f}")

    print("\n" + "=" * 80)
    print("STEP 3: GENERATED NARRATIVE")
    print("=" * 80 + "\n")
    print(narrative)

    print("\n" + "=" * 80)
    print("ASSESSMENT")
    print("=" * 80)

    # Check for required sections
    required = [
        "## Search Summary",
        "## Problem-Solution Mapping",
        "## Technical Pattern",
        "## Implementation Details",
        "## Validation & Outcome",
        "## Search Keywords"
    ]

    for section in required:
        if section in narrative:
            print(f"✅ {section}")
        else:
            print(f"❌ MISSING: {section}")

    # Check keyword density
    keywords_section = narrative.split("## Search Keywords")[-1] if "## Search Keywords" in narrative else ""
    print(f"\n📊 Keyword Analysis:")
    print(f"  Total narrative length: {len(narrative)} chars")
    print(f"  Keyword section length: {len(keywords_section)} chars")
    print(f"  Contains 'Next.js': {'Next.js' in narrative}")
    print(f"  Contains 'TypeScript': {'TypeScript' in narrative or 'typescript' in narrative.lower()}")
    print(f"  Contains 'React': {'React' in narrative or 'react' in narrative.lower()}")

    return {
        'extraction': result,
        'narrative': narrative,
        'tokens': {'input': input_tokens, 'output': output_tokens},
        'cost': cost
    }


if __name__ == "__main__":
    sample = Path("/Users/username/.claude/projects/-Users-username-projects-procsolve-website/637cf8a8-006c-43d1-97c8-998366ecb2fa.jsonl")

    if not sample.exists():
        print(f"Sample not found: {sample}")
        sys.exit(1)

    test_v3_with_skill_v2(sample)
