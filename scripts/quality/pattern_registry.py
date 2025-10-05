#!/usr/bin/env python3
"""
AST-GREP Pattern Registry for Claude Self-Reflect
Based on AST-GREP catalog: https://ast-grep.github.io/catalog

This registry defines patterns for extracting meaningful code patterns
from conversations, replacing generic AST extraction with semantic patterns.
"""

from typing import Dict, List, Any, Set
import re
import logging

logger = logging.getLogger(__name__)

class PatternRegistry:
    """Registry of AST-GREP patterns for code analysis."""

    def __init__(self):
        self.patterns = self._load_patterns()

    def _load_patterns(self) -> Dict[str, List[Dict[str, Any]]]:
        """Load pattern definitions inspired by AST-GREP catalog."""
        return {
            # Python Async Patterns
            "python.async": [
                {
                    "id": "async-context-manager",
                    "pattern": r"async\s+with\s+",
                    "description": "Async context manager (connection pooling, resource management)",
                    "example": "async with pool.get_client() as client"
                },
                {
                    "id": "parallel-execution",
                    "pattern": r"asyncio\.gather",
                    "description": "Parallel async execution",
                    "example": "await asyncio.gather(*tasks)"
                },
                {
                    "id": "streaming-response",
                    "pattern": r"async\s+for\s+\w+\s+in\s+",
                    "description": "Async iteration/streaming",
                    "example": "async for chunk in response"
                },
                {
                    "id": "async-timeout",
                    "pattern": r"async\s+with\s+(?:asyncio\.)?timeout\(",
                    "description": "Async timeout pattern",
                    "example": "async with asyncio.timeout(30)"
                }
            ],

            # Error Handling Patterns
            "error.handling": [
                {
                    "id": "specific-exception",
                    "pattern": r"except\s+(\w+(?:Error|Exception))",
                    "description": "Specific exception handling",
                    "example": "except TimeoutError"
                },
                {
                    "id": "retry-logic",
                    "pattern": r"(?:for|while)\s+\w+\s+in\s+range\([^)]*\).*?try:",
                    "description": "Retry pattern with loop",
                    "example": "for attempt in range(3): try:"
                },
                {
                    "id": "fallback-strategy",
                    "pattern": r"except.*?:\s*(?:return|yield)\s+\w+|except.*?:\s*\w+\s*=",
                    "description": "Fallback on error",
                    "example": "except: return default_value"
                }
            ],

            # React/TypeScript Patterns
            "react.hooks": [
                {
                    "id": "useState-hook",
                    "pattern": r"const\s+\[\s*(\w+),\s*set\w+\s*\]\s*=\s*useState",
                    "description": "React state hook",
                    "example": "const [data, setData] = useState()"
                },
                {
                    "id": "useEffect-hook",
                    "pattern": r"useEffect\(\s*\(\)\s*=>\s*\{",
                    "description": "React effect hook",
                    "example": "useEffect(() => {"
                },
                {
                    "id": "useCallback-memo",
                    "pattern": r"use(?:Callback|Memo)\(\s*(?:\(\)|[^,]+),\s*\[",
                    "description": "React optimization hooks",
                    "example": "useCallback(() => {}, [deps])"
                }
            ],

            # API Patterns
            "api.patterns": [
                {
                    "id": "rest-endpoint",
                    "pattern": r"@(?:app|router)\.(?:get|post|put|delete|patch)\(['\"]([^'\"]+)",
                    "description": "REST API endpoint",
                    "example": "@app.post('/api/users')"
                },
                {
                    "id": "graphql-query",
                    "pattern": r"(?:query|mutation)\s+\w+\s*(?:\([^)]*\))?\s*\{",
                    "description": "GraphQL query/mutation",
                    "example": "query GetUsers {"
                },
                {
                    "id": "websocket-handler",
                    "pattern": r"@(?:socketio|ws)\.on\(['\"](\w+)['\"]",
                    "description": "WebSocket event handler",
                    "example": "@socketio.on('message')"
                }
            ],

            # Architectural Patterns
            "architecture": [
                {
                    "id": "connection-pooling",
                    "pattern": r"(?:Pool|ConnectionPool)\([^)]*\)",
                    "description": "Connection pool initialization",
                    "example": "ConnectionPool(max_size=10)"
                },
                {
                    "id": "singleton-pattern",
                    "pattern": r"@(?:singleton|Singleton)|_instance\s*=\s*None",
                    "description": "Singleton pattern",
                    "example": "@singleton class DatabaseManager"
                },
                {
                    "id": "dependency-injection",
                    "pattern": r"@inject|Depends\(|providers\.(?:Singleton|Factory)",
                    "description": "Dependency injection",
                    "example": "Depends(get_db)"
                },
                {
                    "id": "caching-decorator",
                    "pattern": r"@(?:cache|lru_cache|cached)",
                    "description": "Caching implementation",
                    "example": "@lru_cache(maxsize=128)"
                }
            ],

            # Database Patterns
            "database": [
                {
                    "id": "transaction-context",
                    "pattern": r"(?:async\s+)?with\s+(?:db|conn|session)\.(?:transaction|begin)",
                    "description": "Database transaction",
                    "example": "async with db.transaction()"
                },
                {
                    "id": "bulk-operation",
                    "pattern": r"(?:bulk_|batch_)(?:insert|update|delete)|executemany",
                    "description": "Bulk database operation",
                    "example": "bulk_insert(records)"
                },
                {
                    "id": "query-builder",
                    "pattern": r"\.(?:select|where|join|filter|order_by)\(",
                    "description": "Query builder pattern",
                    "example": "query.where(User.age > 18)"
                }
            ],

            # Testing Patterns
            "testing": [
                {
                    "id": "mock-usage",
                    "pattern": r"@(?:mock|patch)|Mock\(|MagicMock\(",
                    "description": "Mocking in tests",
                    "example": "@patch('module.function')"
                },
                {
                    "id": "parameterized-test",
                    "pattern": r"@pytest\.mark\.parametrize|@parameterized",
                    "description": "Parameterized testing",
                    "example": "@pytest.mark.parametrize('input,expected', [...])"
                },
                {
                    "id": "async-test",
                    "pattern": r"async\s+def\s+test_|@pytest\.mark\.asyncio",
                    "description": "Async test pattern",
                    "example": "async def test_async_function()"
                }
            ]
        }

    def extract_patterns(self, code: str) -> Dict[str, List[str]]:
        """Extract all matching patterns from code."""
        found_patterns = {}

        for category, patterns in self.patterns.items():
            matches = []
            for pattern_def in patterns:
                try:
                    regex = re.compile(pattern_def["pattern"], re.MULTILINE | re.DOTALL)
                    if regex.search(code):
                        matches.append(pattern_def["id"])
                except Exception as e:
                    logger.debug(f"Pattern {pattern_def['id']} failed: {e}")

            if matches:
                found_patterns[category] = matches

        return found_patterns

    def get_pattern_descriptions(self, pattern_ids: List[str]) -> List[str]:
        """Get human-readable descriptions for pattern IDs."""
        descriptions = []
        for category_patterns in self.patterns.values():
            for pattern in category_patterns:
                if pattern["id"] in pattern_ids:
                    descriptions.append(f"{pattern['id']}: {pattern['description']}")
        return descriptions

    def categorize_patterns(self, patterns: Dict[str, List[str]]) -> Dict[str, Any]:
        """Categorize patterns into high-level concepts."""
        categories = {
            "async_architecture": False,
            "error_resilience": False,
            "reactive_ui": False,
            "api_design": False,
            "data_layer": False,
            "testing_practices": False,
            "performance_optimization": False
        }

        # Map patterns to high-level categories
        if "python.async" in patterns:
            categories["async_architecture"] = True
            if any(p in patterns["python.async"] for p in ["parallel-execution", "streaming-response"]):
                categories["performance_optimization"] = True

        if "error.handling" in patterns:
            categories["error_resilience"] = True

        if "react.hooks" in patterns:
            categories["reactive_ui"] = True

        if "api.patterns" in patterns:
            categories["api_design"] = True

        if "database" in patterns:
            categories["data_layer"] = True

        if "testing" in patterns:
            categories["testing_practices"] = True

        if "architecture" in patterns:
            if any(p in patterns["architecture"] for p in ["connection-pooling", "caching-decorator"]):
                categories["performance_optimization"] = True

        return {k: v for k, v in categories.items() if v}


# Singleton instance
_registry = None

def get_registry() -> PatternRegistry:
    """Get or create the singleton pattern registry."""
    global _registry
    if _registry is None:
        _registry = PatternRegistry()
    return _registry


def extract_semantic_patterns(text: str) -> Dict[str, Any]:
    """
    Main entry point for extracting semantic patterns from code.
    Returns structured pattern data suitable for Qdrant metadata.
    """
    registry = get_registry()

    # Extract patterns
    patterns = registry.extract_patterns(text)

    # Get high-level categories
    categories = registry.categorize_patterns(patterns)

    # Flatten for storage
    pattern_list = []
    for category, pattern_ids in patterns.items():
        for pattern_id in pattern_ids:
            pattern_list.append(f"{category}.{pattern_id}")

    return {
        "patterns": pattern_list[:50],  # Limit for storage
        "pattern_categories": list(categories.keys()),
        "has_patterns": len(pattern_list) > 0
    }


if __name__ == "__main__":
    # Test the pattern extraction
    test_code = """
    async def process_data():
        async with pool.get_client() as client:
            try:
                results = await asyncio.gather(
                    fetch_user(client),
                    fetch_posts(client)
                )
            except TimeoutError:
                return cached_data

            async for item in results:
                yield process_item(item)

    const [data, setData] = useState([]);
    useEffect(() => {
        fetchData();
    }, []);
    """

    patterns = extract_semantic_patterns(test_code)
    print("Extracted patterns:", patterns)