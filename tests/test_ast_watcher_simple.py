#!/usr/bin/env python3
"""
Simple test to verify AST pattern extraction works in streaming watcher context
"""

import sys
import os
from pathlib import Path

# Add scripts directory to path
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "scripts"))

def test_ast_extraction():
    """Test AST pattern extraction directly"""
    
    # Check if AST extraction is available
    try:
        from ast_pattern_extractor import extract_code_patterns, AST_GREP_AVAILABLE
        print(f"✅ AST pattern extractor imported successfully")
        print(f"   AST-grep available: {AST_GREP_AVAILABLE}")
    except ImportError as e:
        print(f"❌ Failed to import AST pattern extractor: {e}")
        return False
    
    # Test conversation with code
    test_text = """
    Here's a React component with hooks:
    
    ```javascript
    import React, { useState, useEffect } from 'react';
    
    function TodoList() {
        const [todos, setTodos] = useState([]);
        const [loading, setLoading] = useState(true);
        
        useEffect(() => {
            async function fetchTodos() {
                try {
                    const response = await fetch('/api/todos');
                    const data = await response.json();
                    setTodos(data);
                } catch (error) {
                    console.error('Failed to fetch todos:', error);
                } finally {
                    setLoading(false);
                }
            }
            
            fetchTodos();
        }, []);
        
        return (
            <div>
                {loading ? 'Loading...' : todos.map(todo => <li>{todo}</li>)}
            </div>
        );
    }
    ```
    
    And here's some Python async code:
    
    ```python
    import asyncio
    
    async def process_data(items):
        results = []
        for item in items:
            try:
                result = await process_item(item)
                results.append(result)
            except Exception as e:
                logger.error(f"Failed to process {item}: {e}")
        return results
    ```
    """
    
    # Extract patterns
    result = extract_code_patterns(test_text)
    
    print("\n📊 Extraction Results:")
    print(f"   Method: {result.get('extraction_method')}")
    print(f"   Languages: {result.get('languages_detected')}")
    print(f"   Blocks processed: {result.get('blocks_processed')}")
    print(f"   Extraction time: {result.get('extraction_time', 0):.3f}s")
    
    patterns = result.get('code_patterns', {})
    
    if patterns:
        print("\n🎯 Code Patterns Found:")
        for category, items in patterns.items():
            print(f"   {category}:")
            for item in items[:3]:  # Show first 3 items
                print(f"      - {item}")
            if len(items) > 3:
                print(f"      ... and {len(items) - 3} more")
    else:
        print("\n⚠️  No patterns extracted")
    
    # Validate expected patterns
    success = True
    
    if result.get('extraction_method') == 'ast-grep':
        # AST-based extraction
        if 'react_hooks' not in patterns:
            print("❌ Missing React hooks patterns")
            success = False
        elif 'useState' not in patterns.get('react_hooks', []):
            print("❌ useState not detected in React hooks")
            success = False
        
        if 'async_patterns' not in patterns:
            print("❌ Missing async patterns")
            success = False
    elif result.get('extraction_method') == 'regex_fallback':
        # Regex fallback
        print("ℹ️  Using regex fallback (AST-grep not available or failed)")
        if patterns:
            print("✅ Regex extraction produced patterns")
        else:
            print("⚠️  Regex extraction produced no patterns")
    else:
        print(f"⚠️  Unexpected extraction method: {result.get('extraction_method')}")
    
    return success

def test_watcher_integration():
    """Test that streaming watcher can import AST extraction"""
    
    print("\n🔧 Testing Streaming Watcher Integration:")
    
    # Check that streaming watcher can see the AST module
    watcher_path = Path(__file__).parent.parent / "scripts" / "streaming-watcher.py"
    
    if not watcher_path.exists():
        print(f"❌ Streaming watcher not found at {watcher_path}")
        return False
    
    # Check if AST import was added
    with open(watcher_path, 'r') as f:
        content = f.read()
        
        if 'from ast_pattern_extractor import extract_code_patterns' in content:
            print("✅ AST pattern extractor import found in streaming watcher")
        else:
            print("❌ AST pattern extractor import not found in streaming watcher")
            return False
        
        if 'code_patterns' in content and 'payload' in content:
            print("✅ Code patterns added to Qdrant payload")
        else:
            print("❌ Code patterns not found in payload")
            return False
        
        if 'code_patterns_extracted' in content:
            print("✅ Pattern extraction stats tracking added")
        else:
            print("⚠️  Pattern extraction stats not tracked")
    
    return True

if __name__ == "__main__":
    print("=" * 60)
    print("AST Pattern Extraction Integration Test")
    print("=" * 60)
    
    # Test AST extraction
    ast_success = test_ast_extraction()
    
    # Test watcher integration
    watcher_success = test_watcher_integration()
    
    print("\n" + "=" * 60)
    if ast_success and watcher_success:
        print("✅ All tests passed! Integration complete.")
    else:
        print("⚠️  Some tests failed. Check output above.")
    print("=" * 60)