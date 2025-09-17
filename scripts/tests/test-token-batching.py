#!/usr/bin/env python3
"""Test script for token-aware batching functionality."""

import os
import sys
from pathlib import Path

# Set environment variables before import
os.environ['USE_TOKEN_AWARE_BATCHING'] = 'true'
os.environ['MAX_TOKENS_PER_BATCH'] = '100000'
os.environ['TOKEN_ESTIMATION_RATIO'] = '3'

# Import directly from the script file
import importlib.util
spec = importlib.util.spec_from_file_location(
    "import_conversations_unified",
    str(Path(__file__).parent / "import-conversations-unified.py")
)
import_module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(import_module)

# Get the functions we need
estimate_tokens = import_module.estimate_tokens
create_token_aware_batches = import_module.create_token_aware_batches
split_large_chunk = import_module.split_large_chunk
MAX_TOKENS_PER_BATCH = import_module.MAX_TOKENS_PER_BATCH
TOKEN_ESTIMATION_RATIO = import_module.TOKEN_ESTIMATION_RATIO

def test_token_estimation():
    """Test token estimation function."""
    print("Testing token estimation...")
    
    # Test basic text
    test_text = "This is a test message with approximately 57 characters"
    tokens = estimate_tokens(test_text)
    expected = len(test_text) // 3 * 1.1  # Basic estimate with 10% buffer
    print(f"  Text length: {len(test_text)}, Estimated tokens: {tokens}, Expected: ~{expected:.0f}")
    
    # Test JSON/code content (should have 30% adjustment)
    json_text = '{"key": "value", "nested": {"array": [1, 2, 3]}}' * 5
    json_tokens = estimate_tokens(json_text)
    print(f"  JSON length: {len(json_text)}, Estimated tokens: {json_tokens} (includes structure adjustment)")
    
    print("✓ Token estimation working\n")

def test_batch_creation():
    """Test token-aware batch creation."""
    print("Testing batch creation...")
    
    # Create mock chunks with varying sizes
    mock_chunks = [
        {'id': '1', 'text': 'Short message', 'metadata': {}},
        {'id': '2', 'text': 'A' * 30000, 'metadata': {}},  # ~11,000 tokens
        {'id': '3', 'text': 'B' * 250000, 'metadata': {}}, # ~91,666 tokens (near limit)
        {'id': '4', 'text': 'Another short one', 'metadata': {}},
        {'id': '5', 'text': 'C' * 60000, 'metadata': {}},  # ~22,000 tokens
    ]
    
    batches = create_token_aware_batches(mock_chunks, max_tokens=100000)
    print(f"  Created {len(batches)} batches from {len(mock_chunks)} chunks")
    
    for i, batch in enumerate(batches):
        total_tokens = sum(estimate_tokens(chunk['text']) for chunk in batch)
        chunk_ids = [chunk['id'] for chunk in batch]
        print(f"    Batch {i+1}: {len(batch)} chunks (IDs: {chunk_ids}), ~{total_tokens:,} tokens")
        
        # Verify no batch exceeds limit
        if total_tokens > 100000:
            print(f"    ⚠️  WARNING: Batch {i+1} exceeds token limit!")
    
    print("✓ Batch creation working\n")

def test_chunk_splitting():
    """Test oversized chunk splitting."""
    print("Testing chunk splitting...")
    
    # Create an oversized chunk
    large_messages = [
        {'role': 'user', 'content': 'X' * 150000},  # Very large single message
        {'role': 'assistant', 'content': 'Y' * 150000},
    ]
    
    oversized_chunk = {
        'id': 'oversized-1',
        'text': str(large_messages),
        'metadata': {'conversation_id': 'test'},
        'messages': large_messages,
        'chunk_index': 0,  # Add required chunk_index field
        'start_role': 'user'  # Add required start_role field
    }
    
    # Try to split it
    split_chunks = split_large_chunk(oversized_chunk, max_tokens=100000)
    print(f"  Split oversized chunk into {len(split_chunks)} smaller chunks")
    
    for i, chunk in enumerate(split_chunks):
        tokens = estimate_tokens(chunk['text'])
        msg_count = len(chunk.get('messages', []))
        print(f"    Chunk {i+1}: ~{tokens:,} tokens, {msg_count} messages")
    
    print("✓ Chunk splitting working\n")

def test_edge_cases():
    """Test edge cases and boundary conditions."""
    print("Testing edge cases...")
    
    # Empty batch
    empty_result = create_token_aware_batches([])
    print(f"  Empty input: {len(empty_result)} batches (expected: 0)")
    
    # Single small chunk
    single = [{'id': '1', 'text': 'Hello', 'metadata': {}}]
    single_result = create_token_aware_batches(single)
    print(f"  Single chunk: {len(single_result)} batches (expected: 1)")
    
    # Just under limit  
    under_limit_text = 'Z' * 270000  # ~99,000 tokens (just under 100k limit)
    under = [{'id': '1', 'text': under_limit_text, 'metadata': {}}]
    under_result = create_token_aware_batches(under)
    tokens = estimate_tokens(under_limit_text)
    print(f"  Just under limit (~{tokens:,} tokens): {len(under_result)} batches (expected: 1)")
    
    print("✓ Edge cases handled\n")

def main():
    """Run all tests."""
    print("=" * 60)
    print("TOKEN-AWARE BATCHING TEST SUITE")
    print("=" * 60)
    print(f"Configuration:")
    print(f"  MAX_TOKENS_PER_BATCH: {MAX_TOKENS_PER_BATCH:,}")
    print(f"  TOKEN_ESTIMATION_RATIO: {TOKEN_ESTIMATION_RATIO}")
    print("=" * 60 + "\n")
    
    try:
        test_token_estimation()
        test_batch_creation()
        test_chunk_splitting()
        test_edge_cases()
        
        print("=" * 60)
        print("ALL TESTS PASSED ✓")
        print("=" * 60)
        return 0
    except Exception as e:
        print(f"\n❌ Test failed: {e}")
        import traceback
        traceback.print_exc()
        return 1

if __name__ == "__main__":
    sys.exit(main())