#!/usr/bin/env python3
"""Quick test to capture full narrative output from Claude."""

import os
import sys
from pathlib import Path
from dotenv import load_dotenv

load_dotenv()

try:
    import anthropic
except ImportError:
    print("Error: anthropic SDK not found. Run: pip install anthropic")
    sys.exit(1)

from extract_events import extract_events


def test_narrative_quality(jsonl_path: Path):
    """Test narrative generation and show full output."""

    client = anthropic.Anthropic(api_key=os.getenv("ANTHROPIC_API_KEY"))

    # Read messages
    import json
    messages = []
    with open(jsonl_path) as f:
        for line in f:
            if line.strip():
                messages.append(json.loads(line))

    # Extract events
    print("Extracting events...")
    event_data = extract_events(messages, max_tokens=4000)

    print(f"Original: {event_data['original_message_count']} messages")
    print(f"Compressed: {event_data['estimated_tokens']} tokens")
    print()

    # Call Claude
    system_prompt = """You are a conversation analysis expert specializing in extracting problem-solution narratives from development conversations.

Your task:
1. Analyze the provided event timeline
2. Identify the core problem being solved
3. Track the solution evolution
4. Generate a structured narrative for semantic search

Output format:
## Problem Statement
[Clear problem description]

## Context
[Relevant background]

## Timeline of Events
[Chronological event sequence]

## Solution
[What worked and why]

## Outcome
[Results and validation]

## Keywords
[Search-optimized terms]"""

    print("Calling Claude Sonnet 4.5...")
    response = client.messages.create(
        model="claude-sonnet-4-5-20250929",
        max_tokens=4096,
        system=system_prompt,
        messages=[{
            "role": "user",
            "content": f"""Analyze this conversation event timeline and generate a structured problem-solution narrative:

{event_data['event_timeline']}

Generate the complete narrative following the format specified in the system prompt."""
        }]
    )

    # Extract narrative
    narrative = ""
    for block in response.content:
        if hasattr(block, 'text'):
            narrative += block.text

    print("="*80)
    print("FULL NARRATIVE OUTPUT")
    print("="*80)
    print(narrative)
    print()
    print("="*80)
    print("ANALYSIS")
    print("="*80)

    # Check for required sections
    required_sections = [
        "## Problem Statement",
        "## Context",
        "## Timeline",
        "## Solution",
        "## Outcome",
        "## Keywords"
    ]

    for section in required_sections:
        if section in narrative:
            print(f"✅ {section}")
        else:
            print(f"❌ MISSING: {section}")

    print()
    print(f"Token usage: {response.usage.input_tokens} input, {response.usage.output_tokens} output")
    print(f"Cost: ${(response.usage.input_tokens * 3 + response.usage.output_tokens * 15) / 1_000_000:.6f}")


if __name__ == "__main__":
    # Use the 79-message sample
    sample = Path("/Users/username/.claude/projects/-Users-username-projects-procsolve-website/637cf8a8-006c-43d1-97c8-998366ecb2fa.jsonl")

    if not sample.exists():
        print(f"Sample not found: {sample}")
        sys.exit(1)

    test_narrative_quality(sample)
