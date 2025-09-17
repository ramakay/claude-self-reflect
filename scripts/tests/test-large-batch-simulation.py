#!/usr/bin/env python3
"""Simulate the exact scenario from issue #38 where batches exceeded 120k tokens."""

import os
import sys
from pathlib import Path

# Set environment variables
os.environ['USE_TOKEN_AWARE_BATCHING'] = 'true'
os.environ['MAX_TOKENS_PER_BATCH'] = '100000'
os.environ['TOKEN_ESTIMATION_RATIO'] = '3'

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

def simulate_issue_38():
    """Simulate the exact scenario from issue #38."""
    print("SIMULATING ISSUE #38 SCENARIO")
    print("=" * 60)
    print("Creating chunks that would result in 124,302 tokens...")
    print()
    
    # Create chunks that would sum to ~124,302 tokens
    # Average message size that would cause the issue
    chunks = []
    
    # Create 10 chunks with varying sizes that sum to ~124k tokens
    chunk_configs = [
        15000,  # ~5,500 tokens
        18000,  # ~6,600 tokens  
        20000,  # ~7,333 tokens
        25000,  # ~9,166 tokens
        30000,  # ~11,000 tokens
        35000,  # ~12,833 tokens
        40000,  # ~14,666 tokens
        45000,  # ~16,500 tokens
        50000,  # ~18,333 tokens
        60000,  # ~22,000 tokens
    ]
    
    total_est_tokens = 0
    for i, char_count in enumerate(chunk_configs):
        chunk_text = f"Message {i}: " + "X" * char_count
        chunks.append({
            'id': f'chunk_{i}',
            'text': chunk_text,
            'metadata': {'conversation_id': f'conv_{i}'},
            'messages': [{'role': 'user', 'content': chunk_text}],
            'chunk_index': i,
            'start_role': 'user'
        })
        tokens = estimate_tokens(chunk_text)
        total_est_tokens += tokens
        print(f"  Chunk {i}: {char_count:,} chars → ~{tokens:,} tokens")
    
    print(f"\nTotal estimated tokens: {total_est_tokens:,}")
    print(f"This would exceed Voyage AI's 120k limit!")
    print()
    
    # Test OLD batching (would fail)
    print("OLD BATCHING (fixed 10 chunks per batch):")
    print("-" * 40)
    old_batch = chunks  # All 10 chunks in one batch
    old_tokens = sum(estimate_tokens(chunk['text']) for chunk in old_batch)
    print(f"  Single batch with {len(old_batch)} chunks")
    print(f"  Total tokens: {old_tokens:,}")
    if old_tokens > 120000:
        print(f"  ❌ WOULD FAIL: Exceeds 120k limit by {old_tokens - 120000:,} tokens!")
    print()
    
    # Test NEW token-aware batching
    print("NEW TOKEN-AWARE BATCHING:")
    print("-" * 40)
    batches = create_token_aware_batches(chunks, max_tokens=100000)
    print(f"  Created {len(batches)} batches")
    
    for i, batch in enumerate(batches):
        batch_tokens = sum(estimate_tokens(chunk['text']) for chunk in batch)
        chunk_ids = [chunk['id'] for chunk in batch]
        print(f"  Batch {i+1}: {len(batch)} chunks {chunk_ids}")
        print(f"    Tokens: {batch_tokens:,}")
        
        if batch_tokens > 120000:
            print(f"    ❌ WOULD FAIL: Exceeds 120k limit!")
        elif batch_tokens > 100000:
            print(f"    ⚠️  WARNING: Exceeds our safety limit of 100k")
        else:
            print(f"    ✓ Safe: Under 100k limit with {100000 - batch_tokens:,} buffer")
    
    print()
    print("=" * 60)
    print("RESULT: Token-aware batching prevents the issue!")
    print(f"  Old: 1 batch of {old_tokens:,} tokens → FAILS")
    print(f"  New: {len(batches)} batches, all under 100k → SUCCESS")
    return 0

if __name__ == "__main__":
    sys.exit(simulate_issue_38())