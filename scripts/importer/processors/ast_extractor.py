"""Extract AST elements from code blocks."""

import ast
import re
import logging
from typing import Dict, Any, Set, List

logger = logging.getLogger(__name__)


class ASTExtractor:
    """
    Extract Abstract Syntax Tree elements from code.
    
    Implements the critical fixes identified in code review:
    1. More permissive code fence regex
    2. Python regex fallback for partial code
    3. Bounded extraction with MAX_AST_ELEMENTS
    """
    
    def __init__(self, max_elements: int = 100):
        self.max_elements = max_elements
        
        # FIX: More permissive code fence regex to handle various formats
        # Matches: ```python, ```py, ```javascript, ```ts strict, etc.
        self.code_fence_pattern = re.compile(
            r'```[^\n]*\n?(.*?)```',
            re.DOTALL
        )
        
        # Python patterns for fallback extraction
        self.python_patterns = {
            'function': re.compile(r'^\s*def\s+([A-Za-z_]\w*)\s*\(', re.MULTILINE),
            'async_function': re.compile(r'^\s*async\s+def\s+([A-Za-z_]\w*)\s*\(', re.MULTILINE),
            'class': re.compile(r'^\s*class\s+([A-Za-z_]\w*)\s*[:\(]', re.MULTILINE),
            'method': re.compile(r'^\s+def\s+([A-Za-z_]\w*)\s*\(self', re.MULTILINE),
            'static_method': re.compile(r'@staticmethod.*?\n\s*def\s+([A-Za-z_]\w*)\s*\(', re.MULTILINE | re.DOTALL),
            'class_method': re.compile(r'@classmethod.*?\n\s*def\s+([A-Za-z_]\w*)\s*\(', re.MULTILINE | re.DOTALL)
        }
        
        # JavaScript/TypeScript patterns
        self.js_patterns = {
            'function': re.compile(r'function\s+([A-Za-z_$][\w$]*)\s*\('),
            'arrow': re.compile(r'(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*=\s*(?:\([^)]*\)|[A-Za-z_$][\w$]*)\s*=>'),
            'async_function': re.compile(r'async\s+function\s+([A-Za-z_$][\w$]*)\s*\('),
            'class': re.compile(r'class\s+([A-Za-z_$][\w$]*)\s*(?:extends\s+[A-Za-z_$][\w$]*)?\s*\{'),
            'method': re.compile(r'([A-Za-z_$][\w$]*)\s*\([^)]*\)\s*\{'),
            'export_function': re.compile(r'export\s+(?:default\s+)?(?:async\s+)?function\s+([A-Za-z_$][\w$]*)\s*\('),
            'export_const': re.compile(r'export\s+const\s+([A-Za-z_$][\w$]*)\s*=')
        }
    
    def extract(self, text: str) -> Dict[str, Any]:
        """
        Extract AST elements from text.
        
        Returns:
            Dictionary with ast_elements and code_blocks keys
        """
        elements = set()
        has_code = False
        
        # Extract code blocks using permissive regex
        code_blocks = self.code_fence_pattern.findall(text)
        
        for code_block in code_blocks[:10]:  # Limit processing
            has_code = True
            
            # Try to detect language from content
            if self._looks_like_python(code_block):
                python_elements = self._extract_python_ast(code_block)
                elements.update(python_elements)
            elif self._looks_like_javascript(code_block):
                js_elements = self._extract_javascript_patterns(code_block)
                elements.update(js_elements)
            else:
                # Try both as fallback
                elements.update(self._extract_python_ast(code_block))
                elements.update(self._extract_javascript_patterns(code_block))
            
            # FIX: Enforce max elements limit
            if len(elements) >= self.max_elements:
                logger.debug(f"Reached max AST elements limit: {self.max_elements}")
                break
        
        # Also check for inline code patterns outside of fences
        if not has_code:
            # Look for function/class definitions in plain text
            elements.update(self._extract_inline_patterns(text))
        
        return {
            "ast_elements": list(elements)[:self.max_elements],
            "has_code_blocks": has_code
        }
    
    def _extract_python_ast(self, code: str) -> Set[str]:
        """Extract Python AST elements with fallback to regex."""
        elements = set()
        
        try:
            # Try proper AST parsing first
            tree = ast.parse(code)
            
            for node in ast.walk(tree):
                if len(elements) >= self.max_elements:
                    break
                
                if isinstance(node, ast.FunctionDef):
                    elements.add(f"func:{node.name}")
                elif isinstance(node, ast.AsyncFunctionDef):
                    elements.add(f"func:{node.name}")
                elif isinstance(node, ast.ClassDef):
                    elements.add(f"class:{node.name}")
                    # Extract methods
                    for item in node.body:
                        if isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef)):
                            elements.add(f"method:{node.name}.{item.name}")
                            if len(elements) >= self.max_elements:
                                break
                
        except (SyntaxError, ValueError) as e:
            # FIX: Python regex fallback for partial code fragments
            logger.debug(f"AST parsing failed, using regex fallback: {e}")
            
            for pattern_type, pattern in self.python_patterns.items():
                for match in pattern.finditer(code):
                    if len(elements) >= self.max_elements:
                        break
                    
                    name = match.group(1)
                    if 'method' in pattern_type:
                        elements.add(f"method:{name}")
                    elif 'class' in pattern_type:
                        elements.add(f"class:{name}")
                    else:
                        elements.add(f"func:{name}")
        
        return elements
    
    def _extract_javascript_patterns(self, code: str) -> Set[str]:
        """Extract JavaScript/TypeScript patterns."""
        elements = set()
        
        for pattern_type, pattern in self.js_patterns.items():
            for match in pattern.finditer(code):
                if len(elements) >= self.max_elements:
                    break
                
                name = match.group(1)
                if 'class' in pattern_type:
                    elements.add(f"class:{name}")
                elif 'method' in pattern_type and name not in ['constructor', 'if', 'for', 'while']:
                    elements.add(f"method:{name}")
                else:
                    elements.add(f"func:{name}")
        
        return elements
    
    def _extract_inline_patterns(self, text: str) -> Set[str]:
        """Extract patterns from inline code mentions."""
        elements = set()
        
        # Look for backtick-wrapped function/class names
        inline_pattern = re.compile(r'`([A-Za-z_][\w]*(?:\.[A-Za-z_][\w]*)*)`')
        
        for match in inline_pattern.finditer(text):
            if len(elements) >= self.max_elements:
                break
            
            name = match.group(1)
            # Heuristic: if contains dot, likely a method
            if '.' in name:
                elements.add(f"method:{name}")
            # Heuristic: PascalCase likely a class
            elif name[0].isupper():
                elements.add(f"class:{name}")
            # Otherwise assume function
            else:
                elements.add(f"func:{name}")
        
        return elements
    
    def _looks_like_python(self, code: str) -> bool:
        """Heuristic to detect Python code."""
        python_indicators = [
            'def ', 'import ', 'from ', 'class ', 'self.', 'self,',
            '__init__', '__name__', 'if __name__', 'print(', 'async def'
        ]
        return any(indicator in code for indicator in python_indicators)
    
    def _looks_like_javascript(self, code: str) -> bool:
        """Heuristic to detect JavaScript/TypeScript."""
        js_indicators = [
            'function ', 'const ', 'let ', 'var ', '=>', 'export ',
            'import ', 'class ', 'constructor(', 'this.', 'async function',
            'interface ', 'type ', 'namespace ', 'enum '
        ]
        return any(indicator in code for indicator in js_indicators)