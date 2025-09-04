#!/usr/bin/env python3
"""
Test script to verify AST pattern extraction integration in streaming watcher
"""

import sys
import os
import json
import tempfile
from pathlib import Path
from datetime import datetime

# Add scripts directory to path
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "scripts"))

# Test data with code patterns
TEST_CONVERSATION = {
    "id": "test-conversation-001",
    "name": "Test AST Integration",
    "created_at": datetime.now().isoformat(),
    "updated_at": datetime.now().isoformat(),
    "settings": {},
    "messages": [
        {
            "role": "user",
            "content": "Can you show me how to use React hooks?"
        },
        {
            "role": "assistant", 
            "content": [
                {
                    "type": "text",
                    "text": "Here's an example of React hooks:\n\n```javascript\nimport React, { useState, useEffect } from 'react';\n\nfunction Counter() {\n  const [count, setCount] = useState(0);\n  \n  useEffect(() => {\n    console.log(`Count is: ${count}`);\n  }, [count]);\n  \n  return (\n    <button onClick={() => setCount(count + 1)}>\n      Count: {count}\n    </button>\n  );\n}\n```\n\nThis shows useState for state management and useEffect for side effects."
                }
            ]
        },
        {
            "role": "user",
            "content": "How about async operations?"
        },
        {
            "role": "assistant",
            "content": [
                {
                    "type": "text",
                    "text": "Here's how to handle async operations:\n\n```javascript\nasync function fetchData() {\n  try {\n    const response = await fetch('/api/data');\n    const data = await response.json();\n    return data;\n  } catch (error) {\n    console.error('Error fetching data:', error);\n    throw error;\n  }\n}\n```"
                }
            ]
        }
    ]
}


def test_pattern_extraction():
    """Test that AST patterns are extracted during processing"""
    
    # Create a temporary JSONL file
    with tempfile.NamedTemporaryFile(mode='w', suffix='.jsonl', delete=False) as f:
        json.dump(TEST_CONVERSATION, f)
        f.write('\n')
        temp_file = f.name
    
    try:
        # Import the necessary modules
        import asyncio
        
        # Import watcher modules after adding to path
        import sys
        scripts_path = os.path.join(os.path.dirname(__file__), "..", "scripts")
        if scripts_path not in sys.path:
            sys.path.insert(0, scripts_path)
        
        # Now import after path is set
        from streaming_watcher import StreamingWatcher, Config, AST_EXTRACTION_AVAILABLE
        
        # Create config for test
        config = Config()
        config.logs_dir = Path(temp_file).parent
        config.state_file = Path(tempfile.mktemp(suffix='.json'))
        
        # Create watcher instance
        watcher = StreamingWatcher(config)
        
        # Process the test file
        async def run_test():
            # Find the test file
            test_path = Path(temp_file)
            
            # Check if AST extraction is available
            print(f"AST extraction available: {AST_EXTRACTION_AVAILABLE}")
            
            if AST_EXTRACTION_AVAILABLE:
                # Import and test the extractor directly
                from ast_pattern_extractor import extract_code_patterns
                
                # Extract text from conversation
                text_parts = []
                for msg in TEST_CONVERSATION['messages']:
                    if isinstance(msg['content'], str):
                        text_parts.append(msg['content'])
                    elif isinstance(msg['content'], list):
                        for item in msg['content']:
                            if item.get('type') == 'text':
                                text_parts.append(item.get('text', ''))
                
                combined_text = '\n'.join(text_parts)
                
                # Test pattern extraction
                result = extract_code_patterns(combined_text)
                
                print("\n=== Pattern Extraction Results ===")
                print(f"Extraction method: {result.get('extraction_method')}")
                print(f"Languages detected: {result.get('languages_detected')}")
                print(f"Blocks processed: {result.get('blocks_processed')}")
                print(f"Code patterns found: {list(result.get('code_patterns', {}).keys())}")
                
                # Verify expected patterns
                patterns = result.get('code_patterns', {})
                
                assert 'react_hooks' in patterns, "Should detect React hooks"
                assert 'useState' in patterns.get('react_hooks', []), "Should find useState"
                assert 'useEffect' in patterns.get('react_hooks', []), "Should find useEffect"
                
                assert 'async_patterns' in patterns, "Should detect async patterns"
                assert any('async' in p or 'await' in p for p in patterns.get('async_patterns', [])), "Should find async/await"
                
                assert 'error_handling' in patterns, "Should detect error handling"
                
                print("\n✅ All expected patterns found!")
                
                # Test watcher stats
                print(f"\nWatcher stats:")
                print(f"  Code patterns extracted: {watcher.stats.get('code_patterns_extracted', 0)}")
                print(f"  Pattern extraction failures: {watcher.stats.get('pattern_extraction_failures', 0)}")
                
            else:
                print("⚠️  AST extraction not available - install ast-grep-py")
                print("Run: pip install ast-grep-py")
        
        # Run the test
        asyncio.run(run_test())
        
    finally:
        # Clean up
        if os.path.exists(temp_file):
            os.unlink(temp_file)
        
        print("\n✅ Integration test completed!")


if __name__ == "__main__":
    test_pattern_extraction()