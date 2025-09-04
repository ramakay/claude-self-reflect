#!/usr/bin/env python3
"""
Enhanced Pattern Extractor based on ast-grep catalog
Comprehensive patterns for TypeScript, TSX, Python and JavaScript
"""

import json
import logging
import re
from pathlib import Path
from typing import Dict, List, Any, Optional, Set
from dataclasses import dataclass, field
import ast_grep_py as ast_grep

logger = logging.getLogger(__name__)

@dataclass
class EnhancedPattern:
    """Enhanced pattern definition from ast-grep catalog"""
    id: str
    name: str
    description: str
    language: str
    pattern: str
    category: str
    severity: str = "info"  # info, warning, error
    fix: Optional[str] = None
    examples: List[str] = field(default_factory=list)
    
class EnhancedPatternExtractor:
    """Enhanced pattern extractor using ast-grep catalog patterns"""
    
    # Comprehensive patterns based on ast-grep catalog
    CATALOG_PATTERNS = {
        "typescript": [
            # React Hooks
            {
                "id": "react_hooks_useState",
                "pattern": "useState($$$)",
                "category": "react_hooks",
                "name": "React useState Hook"
            },
            {
                "id": "react_hooks_useEffect", 
                "pattern": "useEffect($$$)",
                "category": "react_hooks",
                "name": "React useEffect Hook"
            },
            {
                "id": "react_hooks_useCallback",
                "pattern": "useCallback($$$)",
                "category": "react_hooks",
                "name": "React useCallback Hook"
            },
            {
                "id": "react_hooks_useMemo",
                "pattern": "useMemo($$$)",
                "category": "react_hooks",
                "name": "React useMemo Hook"
            },
            {
                "id": "react_hooks_useReducer",
                "pattern": "useReducer($$$)",
                "category": "react_hooks",
                "name": "React useReducer Hook"
            },
            {
                "id": "react_hooks_useContext",
                "pattern": "useContext($$$)",
                "category": "react_hooks", 
                "name": "React useContext Hook"
            },
            {
                "id": "react_hooks_useRef",
                "pattern": "useRef($$$)",
                "category": "react_hooks",
                "name": "React useRef Hook"
            },
            {
                "id": "react_hooks_custom",
                "pattern": "function use$NAME($$$) { $$$ }",
                "category": "react_hooks",
                "name": "Custom React Hook"
            },
            
            # Async Patterns
            {
                "id": "async_await_function",
                "pattern": "async function $FUNC($$$) { $$$ }",
                "category": "async_patterns",
                "name": "Async Function"
            },
            {
                "id": "async_arrow_function",
                "pattern": "async ($$$) => $$$",
                "category": "async_patterns",
                "name": "Async Arrow Function"
            },
            {
                "id": "await_expression",
                "pattern": "await $EXPR",
                "category": "async_patterns",
                "name": "Await Expression"
            },
            {
                "id": "promise_then",
                "pattern": "$PROMISE.then($$$)",
                "category": "async_patterns",
                "name": "Promise Then"
            },
            {
                "id": "promise_catch",
                "pattern": "$PROMISE.catch($$$)",
                "category": "async_patterns",
                "name": "Promise Catch"
            },
            {
                "id": "promise_all",
                "pattern": "Promise.all($$$)",
                "category": "async_patterns",
                "name": "Promise.all"
            },
            {
                "id": "promise_race",
                "pattern": "Promise.race($$$)",
                "category": "async_patterns",
                "name": "Promise.race"
            },
            
            # Error Handling
            {
                "id": "try_catch_block",
                "pattern": "try { $$$ } catch ($ERR) { $$$ }",
                "category": "error_handling",
                "name": "Try-Catch Block"
            },
            {
                "id": "try_catch_finally",
                "pattern": "try { $$$ } catch ($ERR) { $$$ } finally { $$$ }",
                "category": "error_handling",
                "name": "Try-Catch-Finally"
            },
            {
                "id": "throw_error",
                "pattern": "throw new Error($$$)",
                "category": "error_handling",
                "name": "Throw Error"
            },
            {
                "id": "throw_expression",
                "pattern": "throw $EXPR",
                "category": "error_handling",
                "name": "Throw Expression"
            },
            
            # API Patterns
            {
                "id": "fetch_api",
                "pattern": "fetch($URL, $$$)",
                "category": "api_patterns",
                "name": "Fetch API"
            },
            {
                "id": "fetch_simple",
                "pattern": "fetch($URL)",
                "category": "api_patterns",
                "name": "Simple Fetch"
            },
            {
                "id": "axios_get",
                "pattern": "axios.get($$$)",
                "category": "api_patterns",
                "name": "Axios GET"
            },
            {
                "id": "axios_post",
                "pattern": "axios.post($$$)",
                "category": "api_patterns",
                "name": "Axios POST"
            },
            
            # Testing Patterns
            {
                "id": "test_describe",
                "pattern": "describe($NAME, $$$)",
                "category": "testing_patterns",
                "name": "Test Describe Block"
            },
            {
                "id": "test_it",
                "pattern": "it($NAME, $$$)",
                "category": "testing_patterns",
                "name": "Test It Block"
            },
            {
                "id": "test_test",
                "pattern": "test($NAME, $$$)",
                "category": "testing_patterns",
                "name": "Test Block"
            },
            {
                "id": "test_expect",
                "pattern": "expect($VAL)",
                "category": "testing_patterns",
                "name": "Expect Assertion"
            },
            {
                "id": "test_beforeEach",
                "pattern": "beforeEach($$$)",
                "category": "testing_patterns",
                "name": "Before Each Hook"
            },
            {
                "id": "test_afterEach",
                "pattern": "afterEach($$$)",
                "category": "testing_patterns",
                "name": "After Each Hook"
            },
            
            # Import/Export Patterns
            {
                "id": "import_default",
                "pattern": "import $NAME from $MODULE",
                "category": "import_patterns",
                "name": "Default Import"
            },
            {
                "id": "import_named",
                "pattern": "import { $$$ } from $MODULE",
                "category": "import_patterns",
                "name": "Named Import"
            },
            {
                "id": "import_namespace",
                "pattern": "import * as $NAME from $MODULE",
                "category": "import_patterns",
                "name": "Namespace Import"
            },
            {
                "id": "export_default",
                "pattern": "export default $EXPR",
                "category": "import_patterns",
                "name": "Default Export"
            },
            {
                "id": "export_named",
                "pattern": "export { $$$ }",
                "category": "import_patterns",
                "name": "Named Export"
            },
            
            # TypeScript Specific
            {
                "id": "interface_declaration",
                "pattern": "interface $NAME { $$$ }",
                "category": "type_patterns",
                "name": "Interface Declaration"
            },
            {
                "id": "type_alias",
                "pattern": "type $NAME = $TYPE",
                "category": "type_patterns",
                "name": "Type Alias"
            },
            {
                "id": "enum_declaration",
                "pattern": "enum $NAME { $$$ }",
                "category": "type_patterns",
                "name": "Enum Declaration"
            },
            {
                "id": "generic_type",
                "pattern": "$TYPE<$$$>",
                "category": "type_patterns",
                "name": "Generic Type"
            },
            
            # Security Patterns
            {
                "id": "input_validation",
                "pattern": "$VAR.validate($$$)",
                "category": "security_patterns",
                "name": "Input Validation"
            },
            {
                "id": "sanitize_input",
                "pattern": "sanitize($INPUT)",
                "category": "security_patterns",
                "name": "Input Sanitization"
            },
            {
                "id": "escape_html",
                "pattern": "escapeHtml($INPUT)",
                "category": "security_patterns",
                "name": "HTML Escaping"
            },
            
            # Performance Patterns
            {
                "id": "memoization",
                "pattern": "memo($FUNC)",
                "category": "performance_patterns",
                "name": "Memoization"
            },
            {
                "id": "lazy_loading",
                "pattern": "lazy(() => import($MODULE))",
                "category": "performance_patterns",
                "name": "Lazy Loading"
            },
            {
                "id": "debounce",
                "pattern": "debounce($FUNC, $DELAY)",
                "category": "performance_patterns",
                "name": "Debounce Function"
            },
            {
                "id": "throttle",
                "pattern": "throttle($FUNC, $DELAY)",
                "category": "performance_patterns",
                "name": "Throttle Function"
            },
            
            # State Management
            {
                "id": "redux_action",
                "pattern": "dispatch($ACTION)",
                "category": "state_patterns",
                "name": "Redux Dispatch"
            },
            {
                "id": "redux_selector",
                "pattern": "useSelector($SELECTOR)",
                "category": "state_patterns",
                "name": "Redux Selector"
            },
            {
                "id": "mobx_observable",
                "pattern": "@observable $VAR",
                "category": "state_patterns",
                "name": "MobX Observable"
            },
            {
                "id": "mobx_action",
                "pattern": "@action $FUNC",
                "category": "state_patterns",
                "name": "MobX Action"
            }
        ],
        
        "tsx": [
            # JSX Patterns
            {
                "id": "jsx_element",
                "pattern": "<$COMPONENT $$$/>",
                "category": "jsx_patterns",
                "name": "JSX Element"
            },
            {
                "id": "jsx_fragment",
                "pattern": "<>$$$</>",
                "category": "jsx_patterns",
                "name": "JSX Fragment"
            },
            {
                "id": "jsx_conditional",
                "pattern": "{$COND ? $TRUE : $FALSE}",
                "category": "jsx_patterns",
                "name": "JSX Conditional"
            },
            {
                "id": "jsx_map",
                "pattern": "{$ARRAY.map($$$)}",
                "category": "jsx_patterns",
                "name": "JSX Map"
            },
            {
                "id": "jsx_short_circuit",
                "pattern": "{$COND && $ELEMENT}",
                "category": "jsx_patterns",
                "name": "JSX Short Circuit"
            },
            {
                "id": "jsx_props_spread",
                "pattern": "<$COMPONENT {...$PROPS}/>",
                "category": "jsx_patterns",
                "name": "Props Spread"
            },
            {
                "id": "jsx_children",
                "pattern": "<$COMPONENT>{$CHILDREN}</$COMPONENT>",
                "category": "jsx_patterns",
                "name": "JSX Children"
            }
        ],
        
        "python": [
            # Python Async
            {
                "id": "py_async_def",
                "pattern": "async def $FUNC($$$): $$$",
                "category": "async_patterns",
                "name": "Async Function"
            },
            {
                "id": "py_await",
                "pattern": "await $EXPR",
                "category": "async_patterns",
                "name": "Await Expression"
            },
            {
                "id": "py_asyncio_run",
                "pattern": "asyncio.run($$$)",
                "category": "async_patterns",
                "name": "Asyncio Run"
            },
            {
                "id": "py_asyncio_gather",
                "pattern": "asyncio.gather($$$)",
                "category": "async_patterns",
                "name": "Asyncio Gather"
            },
            
            # Python Error Handling
            {
                "id": "py_try_except",
                "pattern": "try: $$$ except $EXC: $$$",
                "category": "error_handling",
                "name": "Try-Except"
            },
            {
                "id": "py_try_except_finally",
                "pattern": "try: $$$ except $EXC: $$$ finally: $$$",
                "category": "error_handling",
                "name": "Try-Except-Finally"
            },
            {
                "id": "py_raise",
                "pattern": "raise $EXCEPTION",
                "category": "error_handling",
                "name": "Raise Exception"
            },
            {
                "id": "py_assert",
                "pattern": "assert $CONDITION",
                "category": "error_handling",
                "name": "Assert Statement"
            },
            
            # Python Testing
            {
                "id": "py_pytest_fixture",
                "pattern": "@pytest.fixture",
                "category": "testing_patterns",
                "name": "Pytest Fixture"
            },
            {
                "id": "py_pytest_mark",
                "pattern": "@pytest.mark.$MARK",
                "category": "testing_patterns",
                "name": "Pytest Mark"
            },
            {
                "id": "py_unittest",
                "pattern": "class $TEST(unittest.TestCase): $$$",
                "category": "testing_patterns",
                "name": "Unittest TestCase"
            },
            
            # Python Type Hints
            {
                "id": "py_type_hint",
                "pattern": "$VAR: $TYPE",
                "category": "type_patterns",
                "name": "Type Hint"
            },
            {
                "id": "py_return_hint",
                "pattern": "def $FUNC($$$) -> $TYPE: $$$",
                "category": "type_patterns",
                "name": "Return Type Hint"
            },
            {
                "id": "py_optional",
                "pattern": "Optional[$TYPE]",
                "category": "type_patterns",
                "name": "Optional Type"
            },
            {
                "id": "py_union",
                "pattern": "$TYPE | $TYPE2",
                "category": "type_patterns",
                "name": "Union Type"
            },
            
            # Python Decorators
            {
                "id": "py_decorator",
                "pattern": "@$DECORATOR",
                "category": "decorator_patterns",
                "name": "Decorator"
            },
            {
                "id": "py_property",
                "pattern": "@property",
                "category": "decorator_patterns",
                "name": "Property Decorator"
            },
            {
                "id": "py_staticmethod",
                "pattern": "@staticmethod",
                "category": "decorator_patterns",
                "name": "Static Method"
            },
            {
                "id": "py_classmethod",
                "pattern": "@classmethod",
                "category": "decorator_patterns",
                "name": "Class Method"
            },
            
            # Python Context Managers
            {
                "id": "py_with",
                "pattern": "with $CONTEXT as $VAR: $$$",
                "category": "context_patterns",
                "name": "With Statement"
            },
            {
                "id": "py_contextmanager",
                "pattern": "@contextmanager",
                "category": "context_patterns",
                "name": "Context Manager"
            }
        ]
    }
    
    def __init__(self):
        """Initialize the enhanced pattern extractor"""
        self.patterns_found = {}
        self.stats = {
            "total_patterns": 0,
            "by_category": {},
            "by_language": {}
        }
    
    def extract_patterns(self, code: str, language: str) -> Dict[str, Set[str]]:
        """Extract patterns from code using ast-grep"""
        patterns = {}
        
        # Get patterns for the language
        lang_patterns = self.CATALOG_PATTERNS.get(language, [])
        if language == "javascript":
            # JavaScript uses same patterns as TypeScript
            lang_patterns = self.CATALOG_PATTERNS.get("typescript", [])
        
        # Create ast-grep root
        try:
            root = ast_grep.SgRoot(code, language)
            node = root.root()
        except Exception as e:
            logger.debug(f"Failed to parse code as {language}: {e}")
            return patterns
        
        for pattern_def in lang_patterns:
            try:
                # Try pattern-based matching
                config = {'rule': {'pattern': pattern_def["pattern"]}}
                matches = node.find_all(config)
                
                # If no matches, try simpler pattern (just the function name)
                if not matches and '(' in pattern_def["pattern"]:
                    simple_pattern = pattern_def["pattern"].split('(')[0]
                    config = {'rule': {'pattern': simple_pattern}}
                    matches = node.find_all(config)
                
                if matches:
                    category = pattern_def["category"]
                    if category not in patterns:
                        patterns[category] = set()
                    
                    # Add the pattern name, not the matched code
                    patterns[category].add(pattern_def["name"])
                    
                    # Update stats
                    self.stats["total_patterns"] += 1
                    self.stats["by_category"][category] = self.stats["by_category"].get(category, 0) + 1
                    
            except Exception as e:
                logger.debug(f"Pattern {pattern_def['id']} failed: {e}")
                continue
        
        return patterns
    
    def extract_from_conversation(self, conversation_text: str) -> Dict[str, Any]:
        """Extract patterns from an entire conversation"""
        all_patterns = {}
        code_blocks = self._extract_code_blocks(conversation_text)
        
        for lang, code in code_blocks:
            if lang in ["typescript", "tsx", "javascript", "jsx", "python"]:
                patterns = self.extract_patterns(code, lang)
                
                # Merge patterns
                for category, pattern_set in patterns.items():
                    if category not in all_patterns:
                        all_patterns[category] = set()
                    all_patterns[category].update(pattern_set)
        
        # Convert sets to lists for JSON serialization
        for category in all_patterns:
            all_patterns[category] = list(all_patterns[category])
        
        return {
            "code_patterns": all_patterns,
            "pattern_stats": {
                "total": sum(len(p) for p in all_patterns.values()),
                "categories": len(all_patterns)
            }
        }
    
    def _extract_code_blocks(self, text: str) -> List[tuple]:
        """Extract code blocks from markdown text"""
        code_blocks = []
        pattern = r'```(?:([^\n`]*?)\n)?(.*?)```'
        
        for match in re.finditer(pattern, text, re.DOTALL):
            lang = match.group(1) or ""
            code = match.group(2)
            
            if lang.lower() in ["typescript", "ts", "tsx", "javascript", "js", "jsx", "python", "py"]:
                # Normalize language names
                if lang.lower() in ["ts"]:
                    lang = "typescript"
                elif lang.lower() in ["js"]:
                    lang = "javascript"
                elif lang.lower() in ["py"]:
                    lang = "python"
                
                code_blocks.append((lang.lower(), code))
        
        return code_blocks
    
    def get_summary(self) -> Dict[str, Any]:
        """Get extraction summary"""
        return {
            "total_patterns_found": self.stats["total_patterns"],
            "categories": self.stats["by_category"],
            "languages": self.stats["by_language"],
            "catalog_patterns_used": sum(len(p) for p in self.CATALOG_PATTERNS.values())
        }


if __name__ == "__main__":
    # Test the enhanced extractor
    extractor = EnhancedPatternExtractor()
    
    test_code = '''
    import React, { useState, useEffect, useCallback } from 'react';
    
    const MyComponent = () => {
        const [data, setData] = useState(null);
        
        useEffect(() => {
            async function fetchData() {
                try {
                    const response = await fetch('/api/data');
                    const json = await response.json();
                    setData(json);
                } catch (error) {
                    console.error('Error:', error);
                }
            }
            fetchData();
        }, []);
        
        const handleClick = useCallback(() => {
            console.log('Clicked');
        }, []);
        
        return <div>{data && <p>{data.message}</p>}</div>;
    };
    '''
    
    patterns = extractor.extract_from_conversation(f"```tsx\n{test_code}\n```")
    print(json.dumps(patterns, indent=2))
    print("\nSummary:", extractor.get_summary())