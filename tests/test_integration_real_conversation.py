#!/usr/bin/env python3
"""
Integration test with real conversation to verify AST pattern extraction
"""

import json
import sys
import os
from pathlib import Path
import time

# Add scripts directory to path
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "scripts"))

from ast_pattern_extractor import extract_code_patterns, AST_GREP_AVAILABLE

# Real conversation example from current session
REAL_CONVERSATION = {
    "id": "current-session",
    "messages": [
        {
            "role": "user",
            "content": "Can you help me implement React hooks with error handling?"
        },
        {
            "role": "assistant",
            "content": """I'll help you implement React hooks with proper error handling. Here's a comprehensive example:

```javascript
import React, { useState, useEffect, useCallback } from 'react';

function DataFetcher({ apiUrl }) {
    const [data, setData] = useState(null);
    const [loading, setLoading] = useState(false);
    const [error, setError] = useState(null);
    
    const fetchData = useCallback(async () => {
        setLoading(true);
        setError(null);
        
        try {
            const response = await fetch(apiUrl);
            
            if (!response.ok) {
                throw new Error(`HTTP error! status: ${response.status}`);
            }
            
            const jsonData = await response.json();
            setData(jsonData);
        } catch (err) {
            setError(err.message || 'An unexpected error occurred');
            console.error('Fetch error:', err);
        } finally {
            setLoading(false);
        }
    }, [apiUrl]);
    
    useEffect(() => {
        fetchData();
    }, [fetchData]);
    
    return (
        <div>
            {loading && <p>Loading...</p>}
            {error && <p className="error">Error: {error}</p>}
            {data && <pre>{JSON.stringify(data, null, 2)}</pre>}
        </div>
    );
}
```

And here's a custom error boundary hook:

```javascript
import { useState, useEffect } from 'react';

function useErrorHandler() {
    const [error, setError] = useState(null);
    
    const resetError = () => setError(null);
    
    const captureError = (error) => {
        setError(error);
        // Log to error reporting service
        console.error('Error captured:', error);
    };
    
    useEffect(() => {
        const handleError = (event) => {
            captureError(event.error);
        };
        
        window.addEventListener('error', handleError);
        
        return () => {
            window.removeEventListener('error', handleError);
        };
    }, []);
    
    return { error, resetError, captureError };
}
```"""
        },
        {
            "role": "user", 
            "content": "How about async/await with TypeScript?"
        },
        {
            "role": "assistant",
            "content": """Here's how to use async/await with TypeScript, including proper typing:

```typescript
interface User {
    id: number;
    name: string;
    email: string;
}

interface ApiResponse<T> {
    data: T;
    status: number;
    message?: string;
}

class UserService {
    private baseUrl = 'https://api.example.com';
    
    async getUser(id: number): Promise<User> {
        try {
            const response = await fetch(`${this.baseUrl}/users/${id}`);
            
            if (!response.ok) {
                throw new Error(`Failed to fetch user: ${response.statusText}`);
            }
            
            const data: ApiResponse<User> = await response.json();
            return data.data;
        } catch (error) {
            if (error instanceof Error) {
                console.error('Error fetching user:', error.message);
                throw error;
            }
            throw new Error('Unknown error occurred');
        }
    }
    
    async updateUser(id: number, updates: Partial<User>): Promise<User> {
        try {
            const response = await fetch(`${this.baseUrl}/users/${id}`, {
                method: 'PUT',
                headers: {
                    'Content-Type': 'application/json'
                },
                body: JSON.stringify(updates)
            });
            
            if (!response.ok) {
                throw new Error(`Update failed: ${response.statusText}`);
            }
            
            const data: ApiResponse<User> = await response.json();
            return data.data;
        } catch (error) {
            console.error('Update error:', error);
            throw error;
        }
    }
}
```"""
        }
    ]
}


def test_real_conversation_extraction():
    """Test AST extraction on real conversation data"""
    
    print("=" * 60)
    print("INTEGRATION TEST: Real Conversation AST Extraction")
    print("=" * 60)
    
    if not AST_GREP_AVAILABLE:
        print("⚠️  AST-grep not available, using fallback")
    else:
        print("✅ Using AST-grep for extraction")
    
    # Extract text from conversation
    text_parts = []
    for msg in REAL_CONVERSATION['messages']:
        if isinstance(msg['content'], str):
            text_parts.append(msg['content'])
    
    combined_text = '\n\n'.join(text_parts)
    
    print(f"\n📝 Processing conversation with {len(combined_text)} characters...")
    
    # Extract patterns
    start = time.perf_counter()
    result = extract_code_patterns(combined_text)
    elapsed = time.perf_counter() - start
    
    print(f"\n⏱️  Extraction completed in {elapsed*1000:.2f}ms")
    print(f"   Method: {result.get('extraction_method', 'unknown')}")
    print(f"   Blocks found: {result.get('blocks_processed', 0)}")
    print(f"   Languages: {', '.join(result.get('languages_detected', []))}")
    
    patterns = result.get('code_patterns', {})
    
    if patterns:
        print("\n🎯 Patterns Extracted:")
        for category, items in patterns.items():
            print(f"   {category}:")
            for item in items[:5]:  # Show first 5
                print(f"      - {item}")
            if len(items) > 5:
                print(f"      ... and {len(items) - 5} more")
    
    # Validate expected patterns
    print("\n✅ Validation:")
    
    expected_patterns = {
        'react_hooks': ['useState', 'useEffect', 'useCallback'],
        'async_patterns': ['async/await'],
        'error_handling': ['try/catch'],
        'api_patterns': ['fetch']
    }
    
    all_found = True
    for category, expected_items in expected_patterns.items():
        if category in patterns:
            found_items = patterns[category]
            for item in expected_items:
                if item in found_items:
                    print(f"   ✅ Found {category}: {item}")
                else:
                    print(f"   ❌ Missing {category}: {item}")
                    all_found = False
        else:
            print(f"   ❌ Missing category: {category}")
            all_found = False
    
    # Test metadata structure (as it would be stored in Qdrant)
    print("\n📦 Metadata Structure Test:")
    
    # Simulate what gets stored
    metadata = {
        "conversation_id": REAL_CONVERSATION['id'],
        "code_patterns": patterns,
        "languages_detected": result.get('languages_detected', []),
        "extraction_method": result.get('extraction_method'),
        "extraction_time": elapsed
    }
    
    # Verify JSON serializable
    try:
        json_str = json.dumps(metadata)
        print(f"   ✅ Metadata is JSON serializable ({len(json_str)} bytes)")
    except Exception as e:
        print(f"   ❌ Metadata serialization failed: {e}")
        all_found = False
    
    # Summary
    print("\n" + "=" * 60)
    if all_found:
        print("✅ INTEGRATION TEST PASSED")
        print("   All expected patterns were extracted successfully")
    else:
        print("⚠️  INTEGRATION TEST PARTIALLY PASSED")
        print("   Some patterns were not extracted as expected")
    print("=" * 60)
    
    return all_found


def test_search_utility():
    """Test how extracted patterns improve search utility"""
    
    print("\n" + "=" * 60)
    print("SEARCH UTILITY TEST")
    print("=" * 60)
    
    # Simulate search scenarios
    test_queries = [
        ("React hooks error handling", ["react_hooks", "error_handling"]),
        ("async TypeScript fetch", ["async_patterns", "api_patterns"]),
        ("useState useEffect", ["react_hooks"]),
    ]
    
    # Get patterns from conversation
    text_parts = []
    for msg in REAL_CONVERSATION['messages']:
        if isinstance(msg['content'], str):
            text_parts.append(msg['content'])
    
    combined_text = '\n\n'.join(text_parts)
    result = extract_code_patterns(combined_text)
    patterns = result.get('code_patterns', {})
    
    print("\nTesting search relevance:")
    
    for query, expected_categories in test_queries:
        print(f"\n   Query: '{query}'")
        
        # Check if metadata would help with this query
        relevant_patterns = []
        for category in expected_categories:
            if category in patterns:
                relevant_patterns.extend(patterns[category])
        
        if relevant_patterns:
            print(f"   ✅ Found {len(relevant_patterns)} relevant patterns")
            print(f"      Categories: {', '.join(expected_categories)}")
        else:
            print(f"   ❌ No relevant patterns found")
    
    print("\n✅ Search utility test completed")


if __name__ == "__main__":
    # Run integration test
    success = test_real_conversation_extraction()
    
    # Test search utility
    test_search_utility()
    
    # Exit with appropriate code
    sys.exit(0 if success else 1)