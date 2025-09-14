#!/usr/bin/env python3
"""
AST-GREP Pattern Registry
MANDATORY: Uses AST patterns, not regex patterns
This is the ONLY pattern source for AST-GREP analysis
"""

from typing import Dict, List, Any

class ASTGrepPatternRegistry:
    """
    Pattern registry specifically for ast-grep-py.
    All patterns MUST be valid AST patterns, not regex.
    """

    def __init__(self):
        self.patterns = self._load_ast_patterns()

    def _load_ast_patterns(self) -> Dict[str, List[Dict[str, Any]]]:
        """
        Load AST-GREP specific patterns.
        These are structural patterns, not text patterns.
        """
        return {
            "async_patterns": [
                {
                    "id": "async-function",
                    "pattern": "async def $FUNC($$$): $$$",
                    "description": "Async function definition",
                    "quality": "good",
                    "weight": 2
                },
                {
                    "id": "async-with",
                    "pattern": "async with $RESOURCE: $$$",
                    "description": "Async context manager",
                    "quality": "good",
                    "weight": 3
                },
                {
                    "id": "await-gather",
                    "pattern": "await asyncio.gather($$$)",
                    "description": "Parallel async execution",
                    "quality": "good",
                    "weight": 4
                },
                {
                    "id": "await-call",
                    "pattern": "await $FUNC($$$)",
                    "description": "Awaited async call",
                    "quality": "neutral",
                    "weight": 1
                }
            ],
            "error_handling": [
                {
                    "id": "specific-except",
                    "pattern": "except $ERROR: $$$",
                    "description": "Specific exception handling",
                    "quality": "good",
                    "weight": 3
                },
                {
                    "id": "broad-except",
                    "pattern": "except: $$$",
                    "description": "Bare except clause",
                    "quality": "bad",
                    "weight": -3
                },
                {
                    "id": "try-except",
                    "pattern": "try: $TRY except $ERROR: $EXCEPT",
                    "description": "Try-except block",
                    "quality": "neutral",
                    "weight": 1
                }
            ],
            "logging_patterns": [
                {
                    "id": "logger-call",
                    "pattern": "logger.$METHOD($$$)",
                    "description": "Logger usage",
                    "quality": "good",
                    "weight": 2
                },
                {
                    "id": "print-call",
                    "pattern": "print($$$)",
                    "description": "Print statement",
                    "quality": "bad",
                    "weight": -1
                },
                {
                    "id": "debug-print",
                    "pattern": "print(f$$$)",
                    "description": "F-string print",
                    "quality": "bad",
                    "weight": -2
                }
            ],
            "type_patterns": [
                {
                    "id": "typed-function",
                    "pattern": "def $FUNC($$$) -> $RETURN: $$$",
                    "description": "Function with return type",
                    "quality": "good",
                    "weight": 3
                },
                {
                    "id": "typed-async",
                    "pattern": "async def $FUNC($$$) -> $RETURN: $$$",
                    "description": "Async function with return type",
                    "quality": "good",
                    "weight": 4
                }
            ],
            "import_patterns": [
                {
                    "id": "import-from",
                    "pattern": "from $MODULE import $$$",
                    "description": "From import",
                    "quality": "neutral",
                    "weight": 0
                },
                {
                    "id": "import-as",
                    "pattern": "import $MODULE as $ALIAS",
                    "description": "Import with alias",
                    "quality": "neutral",
                    "weight": 0
                }
            ],
            "anti_patterns": [
                {
                    "id": "sync-sleep",
                    "pattern": "time.sleep($$$)",
                    "description": "Blocking sleep",
                    "quality": "bad",
                    "weight": -5
                },
                {
                    "id": "sync-open",
                    "pattern": "open($$$)",
                    "description": "Sync file open",
                    "quality": "bad",
                    "weight": -3
                },
                {
                    "id": "requests-call",
                    "pattern": "requests.$METHOD($$$)",
                    "description": "Sync HTTP request",
                    "quality": "bad",
                    "weight": -4
                },
                {
                    "id": "global-var",
                    "pattern": "global $VAR",
                    "description": "Global variable",
                    "quality": "bad",
                    "weight": -2
                }
            ],
            "qdrant_patterns": [
                {
                    "id": "qdrant-search",
                    "pattern": "$CLIENT.search($$$)",
                    "description": "Qdrant search operation",
                    "quality": "neutral",
                    "weight": 1
                },
                {
                    "id": "qdrant-upsert",
                    "pattern": "$CLIENT.upsert($$$)",
                    "description": "Qdrant upsert operation",
                    "quality": "neutral",
                    "weight": 1
                },
                {
                    "id": "collection-create",
                    "pattern": "create_collection($$$)",
                    "description": "Collection creation",
                    "quality": "neutral",
                    "weight": 1
                }
            ],
            "mcp_patterns": [
                {
                    "id": "mcp-tool",
                    "pattern": "@server.tool\nasync def $TOOL($$$): $$$",
                    "description": "MCP tool definition",
                    "quality": "good",
                    "weight": 5
                },
                {
                    "id": "mcp-function",
                    "pattern": "@mcp.tool($$$)\nasync def $TOOL($$$): $$$",
                    "description": "MCP tool with decorator",
                    "quality": "good",
                    "weight": 5
                }
            ]
        }

    def get_all_patterns(self) -> List[Dict[str, Any]]:
        """Get all patterns as a flat list."""
        all_patterns = []
        for category, patterns in self.patterns.items():
            for pattern in patterns:
                pattern['category'] = category
                all_patterns.append(pattern)
        return all_patterns

    def get_good_patterns(self) -> List[Dict[str, Any]]:
        """Get only good quality patterns."""
        return [p for p in self.get_all_patterns() if p.get('quality') == 'good']

    def get_bad_patterns(self) -> List[Dict[str, Any]]:
        """Get only bad quality patterns."""
        return [p for p in self.get_all_patterns() if p.get('quality') == 'bad']

    def calculate_quality_score(self, matches: List[Dict]) -> float:
        """
        Calculate quality score based on pattern matches.
        Each match includes the pattern and count.
        """
        total_weight = 0
        total_count = 0

        for match in matches:
            weight = match.get('weight', 0)
            count = match.get('count', 0)
            total_weight += weight * count
            total_count += abs(weight) * count

        if total_count == 0:
            return 0.5

        # Normalize to 0-1 range
        # Assuming max weight sum of 100 and min of -100
        normalized = (total_weight + 100) / 200
        return max(0.0, min(1.0, normalized))


# Singleton instance
_registry = None

def get_ast_grep_registry() -> ASTGrepPatternRegistry:
    """Get or create the AST-GREP pattern registry."""
    global _registry
    if _registry is None:
        _registry = ASTGrepPatternRegistry()
    return _registry


if __name__ == "__main__":
    # Test the registry
    registry = get_ast_grep_registry()

    print("AST-GREP Pattern Registry")
    print("=" * 60)

    print(f"\nTotal patterns: {len(registry.get_all_patterns())}")
    print(f"Good patterns: {len(registry.get_good_patterns())}")
    print(f"Bad patterns: {len(registry.get_bad_patterns())}")

    print("\nCategories:")
    for category in registry.patterns.keys():
        count = len(registry.patterns[category])
        print(f"  - {category}: {count} patterns")

    print("\nSample patterns:")
    for pattern in registry.get_all_patterns()[:5]:
        print(f"  - {pattern['id']}: {pattern['pattern'][:30]}...")
        print(f"    Quality: {pattern['quality']}, Weight: {pattern['weight']}")