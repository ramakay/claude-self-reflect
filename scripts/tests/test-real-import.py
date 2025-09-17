#!/usr/bin/env python3
"""Test the token-aware batching with real conversation files."""

import os
import sys
import json
import glob
from pathlib import Path

# Set environment variables
os.environ['USE_TOKEN_AWARE_BATCHING'] = 'true'
os.environ['MAX_TOKENS_PER_BATCH'] = '100000'
os.environ['TOKEN_ESTIMATION_RATIO'] = '3'
os.environ['LOG_LEVEL'] = 'DEBUG'

# Import the module
import importlib.util
spec = importlib.util.spec_from_file_location(
    "import_conversations_unified",
    str(Path(__file__).parent / "import-conversations-unified.py")
)
import_module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(import_module)

# Get functions
estimate_tokens = import_module.estimate_tokens
create_token_aware_batches = import_module.create_token_aware_batches

def test_with_real_file():
    """Test with an actual conversation file."""
    test_file = os.path.expanduser("~/.claude/projects/claude-self-reflect-stress-test/stress-test-large-2.json")
    
    if not os.path.exists(test_file):
        print(f"Test file not found: {test_file}")
        return 1
    
    print(f"Testing with real file: {test_file}")
    print("=" * 60)
    
    # Read the file
    with open(test_file, 'r') as f:
        data = json.load(f)
    
    # Get basic stats
    messages = data.get('messages', [])
    total_chars = sum(len(msg.get('content', '')) for msg in messages)
    estimated_tokens = estimate_tokens(json.dumps(data))
    
    print(f"File stats:")
    print(f"  Messages: {len(messages)}")
    print(f"  Total characters: {total_chars:,}")
    print(f"  Estimated tokens: {estimated_tokens:,}")
    print()
    
    # Process into chunks (mock the chunking logic)
    chunks = []
    chunk_size = 10  # Same as in the actual script
    
    for i in range(0, len(messages), chunk_size):
        chunk_messages = messages[i:i+chunk_size]
        chunk_text = json.dumps(chunk_messages)
        chunks.append({
            'id': f'chunk_{i//chunk_size}',
            'text': chunk_text,
            'metadata': {'file': test_file},
            'messages': chunk_messages,
            'chunk_index': i // chunk_size,
            'start_role': chunk_messages[0].get('role', 'unknown') if chunk_messages else 'unknown'
        })
    
    print(f"Created {len(chunks)} chunks from messages")
    
    # Test token-aware batching
    batches = create_token_aware_batches(chunks, max_tokens=100000)
    
    print(f"\nToken-aware batching results:")
    print(f"  Total batches: {len(batches)}")
    
    for i, batch in enumerate(batches):
        total_tokens = sum(estimate_tokens(chunk['text']) for chunk in batch)
        print(f"  Batch {i+1}: {len(batch)} chunks, ~{total_tokens:,} tokens")
        
        # Verify no batch exceeds limit
        if total_tokens > 100000:
            print(f"    ⚠️  WARNING: Batch exceeds token limit!")
    
    # Check if we would have had issues with old batching
    old_batch_size = 10  # chunks per batch in old system
    old_batches = [chunks[i:i+old_batch_size] for i in range(0, len(chunks), old_batch_size)]
    
    print(f"\nComparison with old fixed batching:")
    print(f"  Old system would create {len(old_batches)} batches")
    
    for i, batch in enumerate(old_batches):
        total_tokens = sum(estimate_tokens(chunk['text']) for chunk in batch)
        print(f"  Old Batch {i+1}: {len(batch)} chunks, ~{total_tokens:,} tokens")
        if total_tokens > 120000:
            print(f"    ❌ WOULD FAIL: Exceeds Voyage AI 120k limit!")
        elif total_tokens > 100000:
            print(f"    ⚠️  At risk: Close to limit")
    
    print("\n" + "=" * 60)
    print("✓ Test completed successfully")
    return 0

if __name__ == "__main__":
    sys.exit(test_with_real_file())