#!/usr/bin/env python3
"""
Unit tests for AST pattern extractor
"""

import os
import sys
import pytest
import time
from unittest.mock import patch, MagicMock

# Add scripts directory to path
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "scripts"))

from ast_pattern_extractor import (
    extract_code_blocks,
    detect_language,
    extract_patterns_from_code,
    extract_code_patterns,
    extract_code_patterns_with_fallback,
    extract_patterns_with_regex,
    _empty_result,
    AST_GREP_AVAILABLE,
)


class TestCodeBlockExtraction:
    """Test code block extraction from text"""
    
    def test_extract_markdown_code_blocks(self):
        """Test extracting markdown code blocks with language"""
        text = """
        Here's some code:
        ```python
        def hello():
            print("world")
        ```
        
        And some JavaScript:
        ```javascript
        const x = 5;
        ```
        """
        
        blocks = extract_code_blocks(text)
        assert len(blocks) == 2
        assert blocks[0]["language"] == "python"
        assert "def hello" in blocks[0]["code"]
        assert blocks[1]["language"] == "javascript"
        assert "const x" in blocks[1]["code"]
    
    def test_extract_code_blocks_without_language(self):
        """Test extracting code blocks without specified language"""
        text = """
        ```
        import React from 'react';
        ```
        """
        
        blocks = extract_code_blocks(text)
        assert len(blocks) == 1
        # Should detect as jsx/tsx based on content
        assert blocks[0]["language"] in ["jsx", "tsx"]
    
    def test_extract_code_blocks_with_c_plus_plus(self):
        """Test extracting C++ code blocks"""
        text = """
        ```c++
        #include <iostream>
        std::cout << "Hello";
        ```
        """
        
        blocks = extract_code_blocks(text)
        assert len(blocks) == 1
        # c++ gets detected and normalized to cpp
        assert blocks[0]["language"] in ["c++", "cpp"]
    
    def test_extract_inline_code(self):
        """Test extracting substantial inline code"""
        text = """
        Here's a long inline: `function test() {
            return "this is multiline";
        }`
        """
        
        blocks = extract_code_blocks(text)
        assert len(blocks) == 1
        assert "function test" in blocks[0]["code"]


class TestLanguageDetection:
    """Test language detection heuristics"""
    
    def test_detect_python(self):
        code = "import numpy as np\ndef main():\n    print('hello')"
        assert detect_language(code) == "python"
    
    def test_detect_javascript(self):
        code = "const x = 5;\nconsole.log(x);"
        assert detect_language(code) == "javascript"
    
    def test_detect_typescript(self):
        code = "interface User {\n  name: string;\n}"
        assert detect_language(code) == "typescript"
    
    def test_detect_react(self):
        code = "import React from 'react';\n<Component />"
        assert detect_language(code) in ["jsx", "tsx"]
    
    def test_detect_cpp(self):
        code = "#include <iostream>\nstd::cout << 'test';"
        assert detect_language(code) == "cpp"
    
    def test_detect_unknown(self):
        code = "random text without code patterns"
        assert detect_language(code) is None


@pytest.mark.skipif(not AST_GREP_AVAILABLE, reason="ast-grep-py not available")
class TestASTPatternExtraction:
    """Test AST-based pattern extraction"""
    
    def test_extract_react_hooks(self):
        """Test extracting React hooks patterns"""
        code = """
        import { useState, useEffect } from 'react';
        
        function Component() {
            const [count, setCount] = useState(0);
            
            useEffect(() => {
                console.log(count);
            }, [count]);
        }
        """
        
        patterns = extract_patterns_from_code(code, "javascript")
        assert "react_hooks" in patterns
        assert "useState" in patterns["react_hooks"]
        assert "useEffect" in patterns["react_hooks"]
    
    def test_extract_async_patterns(self):
        """Test extracting async/await patterns"""
        code = """
        async function fetchData() {
            try {
                const response = await fetch('/api');
                return await response.json();
            } catch (error) {
                console.error(error);
            }
        }
        """
        
        patterns = extract_patterns_from_code(code, "javascript")
        assert "async_patterns" in patterns
        assert "async/await" in patterns["async_patterns"]
    
    def test_extract_error_handling(self):
        """Test extracting error handling patterns"""
        code = """
        try {
            doSomething();
        } catch (error) {
            throw new Error('Failed');
        }
        """
        
        patterns = extract_patterns_from_code(code, "javascript")
        assert "error_handling" in patterns
        assert any("try" in p or "catch" in p for p in patterns["error_handling"])
    
    def test_language_restriction(self):
        """Test that patterns are restricted to relevant languages"""
        python_code = """
        def test():
            pass
        """
        
        # React hooks shouldn't be detected in Python code
        patterns = extract_patterns_from_code(python_code, "python")
        assert "react_hooks" not in patterns
    
    def test_timeout_protection(self):
        """Test timeout protection for long-running patterns"""
        # Create a very large code block
        large_code = "const x = 1;\n" * 10000
        
        with patch("ast_pattern_extractor.AST_TIMEOUT_SECONDS", 0.001):
            patterns = extract_patterns_from_code(large_code, "javascript")
            # Should return empty on timeout
            assert patterns == {}


class TestMainExtraction:
    """Test the main extraction function"""
    
    def test_extract_code_patterns_full_flow(self):
        """Test full extraction flow"""
        conversation = """
        Let me show you a React component:
        
        ```javascript
        import React, { useState } from 'react';
        
        function Counter() {
            const [count, setCount] = useState(0);
            
            return (
                <button onClick={() => setCount(count + 1)}>
                    Count: {count}
                </button>
            );
        }
        ```
        """
        
        result = extract_code_patterns(conversation)
        
        assert result["extraction_method"] in ["ast-grep", "regex_fallback"]
        # JavaScript with React gets correctly detected as JSX
        assert any(lang in result["languages_detected"] for lang in ["javascript", "jsx", "tsx"])
        assert result["blocks_processed"] == 1
        
        if result["extraction_method"] == "ast-grep":
            assert "react_hooks" in result["code_patterns"]
            assert "useState" in result["code_patterns"]["react_hooks"]
    
    def test_extract_with_size_limits(self):
        """Test that oversized blocks are skipped"""
        # Create a conversation with an oversized block
        large_code = "x = 1\n" * 3000  # Exceeds MAX_CODE_LINES
        conversation = f"```python\n{large_code}```"
        
        with patch("ast_pattern_extractor.MAX_CODE_LINES", 2000):
            result = extract_code_patterns(conversation)
            assert result["blocks_processed"] == 0
    
    def test_extract_multiple_languages(self):
        """Test extracting patterns from multiple languages"""
        conversation = """
        Python code:
        ```python
        async def fetch():
            await something()
        ```
        
        JavaScript code:
        ```javascript
        async function fetch() {
            await something();
        }
        ```
        """
        
        result = extract_code_patterns(conversation)
        
        if result["extraction_method"] == "ast-grep":
            assert "python" in result["languages_detected"]
            assert "javascript" in result["languages_detected"]
    
    def test_empty_result_consistency(self):
        """Test that empty results have consistent structure"""
        result = _empty_result("test_method", ["python"], 5, 1.234)
        
        assert result["code_patterns"] == {}
        assert result["languages_detected"] == ["python"]
        assert result["extraction_method"] == "test_method"
        assert result["extraction_time"] == 1.234
        assert result["blocks_processed"] == 5
    
    def test_no_code_found(self):
        """Test handling when no code blocks are found"""
        conversation = "This is just text without any code"
        
        result = extract_code_patterns(conversation)
        
        assert result["extraction_method"] == "no_code_found"
        assert result["code_patterns"] == {}
        assert result["blocks_processed"] == 0


class TestFallbackExtraction:
    """Test regex fallback extraction"""
    
    def test_regex_fallback(self):
        """Test regex-based pattern extraction"""
        text = """
        useState(0);
        async function test() {
            await fetch('/api');
        }
        try {
            something();
        } catch (e) {}
        """
        
        result = extract_patterns_with_regex(text)
        
        assert "react_hooks" in result["code_patterns"]
        assert "async_patterns" in result["code_patterns"]
        assert "error_handling" in result["code_patterns"]
        assert result["extraction_method"] == "regex_fallback"
    
    def test_fallback_on_ast_failure(self):
        """Test fallback when AST extraction fails"""
        conversation = "```invalid\nsome invalid code```"
        
        with patch("ast_pattern_extractor.AST_GREP_ENABLED", False):
            result = extract_code_patterns_with_fallback(conversation)
            
            # Should fall back to regex
            assert result["extraction_method"] in ["regex_fallback", "no_code_found"]


class TestPerformance:
    """Performance and benchmark tests"""
    
    def test_extraction_performance(self):
        """Test that extraction completes within timeout"""
        conversation = """
        ```javascript
        import React, { useState, useEffect } from 'react';
        
        function App() {
            const [data, setData] = useState(null);
            
            useEffect(() => {
                async function fetchData() {
                    try {
                        const response = await fetch('/api');
                        setData(await response.json());
                    } catch (error) {
                        console.error(error);
                    }
                }
                fetchData();
            }, []);
            
            return <div>{data}</div>;
        }
        ```
        """
        
        start = time.time()
        result = extract_code_patterns(conversation)
        elapsed = time.time() - start
        
        # Should complete within 2 seconds
        assert elapsed < 2.0
        
        if result.get("extraction_time"):
            assert result["extraction_time"] < 2.0
    
    def test_max_blocks_limit(self):
        """Test that max_blocks parameter is respected"""
        # Create conversation with many code blocks
        blocks = [f"```python\ncode{i} = {i}```" for i in range(20)]
        conversation = "\n".join(blocks)
        
        result = extract_code_patterns(conversation, max_blocks=5)
        
        # Should process at most 5 blocks
        assert result["blocks_processed"] <= 5


@pytest.mark.skipif(not AST_GREP_AVAILABLE, reason="ast-grep-py not available")
class TestPlatformCompatibility:
    """Test platform-specific features"""
    
    def test_timeout_on_windows(self):
        """Test that timeout degrades gracefully on Windows"""
        with patch("os.name", "nt"):
            # Should not crash on Windows
            code = "const x = 5;"
            patterns = extract_patterns_from_code(code, "javascript")
            # Might return empty or actual patterns depending on implementation
            assert isinstance(patterns, dict)
    
    def test_signal_not_available(self):
        """Test handling when SIGALRM is not available"""
        with patch("signal.SIGALRM", new=None, create=True):
            code = "const x = 5;"
            patterns = extract_patterns_from_code(code, "javascript")
            assert isinstance(patterns, dict)


if __name__ == "__main__":
    pytest.main([__file__, "-v"])