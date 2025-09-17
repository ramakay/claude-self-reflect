#!/usr/bin/env python3
"""
Unified AST-GREP Pattern Registry
Combines custom patterns with official catalog patterns
MANDATORY: Uses AST patterns only, no regex
"""

from typing import Dict, List, Any
import json
import logging
from pathlib import Path

# Setup logger
logger = logging.getLogger(__name__)
logging.basicConfig(level=logging.INFO)

class UnifiedASTGrepRegistry:
    """
    Unified registry combining:
    1. Custom AST patterns for Python
    2. Official catalog patterns from AST-GREP
    3. TypeScript/JavaScript patterns
    All patterns are AST-based, not regex.
    """

    def __init__(self):
        self.patterns = self._load_unified_patterns()

        # Merge auto-updated catalog if present
        json_path = Path(__file__).parent / "unified_registry.json"
        if json_path.exists():
            try:
                with open(json_path, 'r') as f:
                    data = json.load(f)
                    # Merge catalog patterns into existing patterns
                    catalog_patterns = data.get("patterns", {})
                    for category, patterns in catalog_patterns.items():
                        if category not in self.patterns:
                            self.patterns[category] = []
                        # Add patterns that don't already exist
                        existing_ids = {p['id'] for p in self.patterns[category]}
                        for pattern in patterns:
                            if pattern.get('id') not in existing_ids:
                                self.patterns[category].append(pattern)
            except Exception as e:
                # Continue with static patterns if catalog load fails
                pass

    def _load_unified_patterns(self) -> Dict[str, List[Dict[str, Any]]]:
        """Load unified patterns from multiple sources."""
        patterns = {}

        # Python patterns (custom)
        patterns.update(self._load_python_patterns())

        # TypeScript patterns (from catalog)
        patterns.update(self._load_typescript_patterns())

        # JavaScript patterns (shared with TS)
        patterns.update(self._load_javascript_patterns())

        # Shell script patterns
        patterns.update(self._load_shell_patterns())

        return patterns

    def _load_python_patterns(self) -> Dict[str, List[Dict[str, Any]]]:
        """Python-specific AST patterns."""
        return {
            "python_async": [
                {
                    "id": "async-function",
                    "pattern": "async def $FUNC($$$): $$$",
                    "description": "Async function definition",
                    "quality": "good",
                    "weight": 2,
                    "language": "python"
                },
                {
                    "id": "async-with",
                    "pattern": "async with $RESOURCE: $$$",
                    "description": "Async context manager",
                    "quality": "good",
                    "weight": 3,
                    "language": "python"
                },
                {
                    "id": "await-gather",
                    "pattern": "await asyncio.gather($$$)",
                    "description": "Parallel async execution",
                    "quality": "good",
                    "weight": 4,
                    "language": "python"
                },
                {
                    "id": "await-call",
                    "pattern": "await $FUNC($$$)",
                    "description": "Awaited async call",
                    "quality": "neutral",
                    "weight": 1,
                    "language": "python"
                }
            ],
            "python_error_handling": [
                {
                    "id": "specific-except",
                    "pattern": "except $ERROR: $$$",
                    "description": "Specific exception handling",
                    "quality": "good",
                    "weight": 3,
                    "language": "python"
                },
                {
                    "id": "broad-except",
                    "pattern": "except: $$$",
                    "description": "Bare except clause",
                    "quality": "bad",
                    "weight": -3,
                    "language": "python"
                },
                {
                    "id": "try-finally",
                    "pattern": "try: $TRY finally: $FINALLY",
                    "description": "Try-finally block",
                    "quality": "good",
                    "weight": 2,
                    "language": "python"
                }
            ],
            "python_logging": [
                {
                    "id": "logger-call",
                    "pattern": "logger.$METHOD($$$)",
                    "description": "Logger usage",
                    "quality": "good",
                    "weight": 2,
                    "language": "python"
                },
                {
                    "id": "print-call",
                    "pattern": "print($$$)",
                    "description": "Print statement",
                    "quality": "bad",
                    "weight": -1,
                    "language": "python"
                },
                {
                    "id": "debug-print-f-sq",
                    "pattern": "print(f'$A')",
                    "description": "F-string print (single quote)",
                    "quality": "bad",
                    "weight": -2,
                    "language": "python"
                },
                {
                    "id": "debug-print-f-dq",
                    "pattern": "print(f\"$A\")",
                    "description": "F-string print (double quote)",
                    "quality": "bad",
                    "weight": -2,
                    "language": "python"
                }
            ],
            "python_typing": [
                {
                    "id": "typed-function",
                    "pattern": "def $FUNC($$$) -> $RETURN: $$$",
                    "description": "Function with return type",
                    "quality": "good",
                    "weight": 3,
                    "language": "python"
                },
                {
                    "id": "typed-async",
                    "pattern": "async def $FUNC($$$) -> $RETURN: $$$",
                    "description": "Async function with return type",
                    "quality": "good",
                    "weight": 4,
                    "language": "python"
                },
                {
                    "id": "type-annotation",
                    "pattern": "$VAR: $TYPE = $$$",
                    "description": "Variable type annotation",
                    "quality": "good",
                    "weight": 2,
                    "language": "python"
                }
            ],
            "python_antipatterns": [
                {
                    "id": "sync-sleep",
                    "pattern": "time.sleep($$$)",
                    "description": "Blocking sleep in async context",
                    "quality": "bad",
                    "weight": -5,
                    "language": "python"
                },
                {
                    "id": "sync-open",
                    "pattern": "open($$$)",
                    "description": "Sync file open (should use aiofiles)",
                    "quality": "bad",
                    "weight": -3,
                    "language": "python"
                },
                {
                    "id": "requests-call",
                    "pattern": "requests.$METHOD($$$)",
                    "description": "Sync HTTP request (should use aiohttp)",
                    "quality": "bad",
                    "weight": -4,
                    "language": "python"
                },
                {
                    "id": "global-var",
                    "pattern": "global $VAR",
                    "description": "Global variable usage",
                    "quality": "bad",
                    "weight": -2,
                    "language": "python"
                },
                {
                    "id": "mutable-default",
                    "pattern": "def $FUNC($$$, $ARG=[]): $$$",
                    "description": "Mutable default argument",
                    "quality": "bad",
                    "weight": -4,
                    "language": "python"
                },
                {
                    "id": "sync-voyage-embed",
                    "pattern": "$CLIENT.embed($$$)",
                    "description": "Blocking Voyage embed in async context",
                    "quality": "bad",
                    "weight": -5,
                    "language": "python",
                    "inside": "async def $FUNC($$$): $$$"
                },
                {
                    "id": "thread-join-async",
                    "pattern": "$THREAD.join($$$)",
                    "description": "Thread join blocking async context",
                    "quality": "bad",
                    "weight": -5,
                    "language": "python",
                    "inside": "async def $FUNC($$$): $$$"
                },
                {
                    "id": "invalid-env-var-hyphen",
                    "pattern": "os.getenv('$VAR')",
                    "description": "Environment variable with hyphen (invalid in shells)",
                    "quality": "bad",
                    "weight": -3,
                    "language": "python",
                    "constraint": "$VAR matches .*-.*"
                },
                {
                    "id": "dotenv-override-runtime",
                    "pattern": "load_dotenv($$$, override=True)",
                    "description": "Runtime environment mutation in MCP",
                    "quality": "bad",
                    "weight": -3,
                    "language": "python"
                }
            ],
            "python_qdrant": [
                {
                    "id": "qdrant-search",
                    "pattern": "$CLIENT.search($$$)",
                    "description": "Qdrant search operation",
                    "quality": "neutral",
                    "weight": 1,
                    "language": "python"
                },
                {
                    "id": "qdrant-upsert",
                    "pattern": "$CLIENT.upsert($$$)",
                    "description": "Qdrant upsert operation",
                    "quality": "neutral",
                    "weight": 1,
                    "language": "python"
                },
                {
                    "id": "collection-create",
                    "pattern": "create_collection($$$)",
                    "description": "Collection creation",
                    "quality": "neutral",
                    "weight": 1,
                    "language": "python"
                }
            ],
            "python_mcp": [
                {
                    "id": "mcp-tool",
                    "pattern": "@server.tool\nasync def $TOOL($$$): $$$",
                    "description": "MCP tool definition",
                    "quality": "good",
                    "weight": 5,
                    "language": "python"
                },
                {
                    "id": "mcp-resource",
                    "pattern": "@server.resource($$$)\nasync def $RESOURCE($$$): $$$",
                    "description": "MCP resource definition",
                    "quality": "good",
                    "weight": 5,
                    "language": "python"
                },
                {
                    "id": "missing-embedding-guard",
                    "pattern": "query_embedding = await $MGR.generate_embedding($$$)\n$$$\nawait $CLIENT.search($$$, query_vector=query_embedding, $$$)",
                    "description": "Missing None check after embedding generation",
                    "quality": "bad",
                    "weight": -4,
                    "language": "python"
                },
                {
                    "id": "attr-vs-api",
                    "pattern": "$MGR.model_name",
                    "description": "Accessing non-existent attribute instead of API",
                    "quality": "bad",
                    "weight": -3,
                    "language": "python",
                    "note": "Use get_model_info() instead"
                },
                {
                    "id": "duplicate-import",
                    "pattern": "import $MODULE\n$$$\ndef $FUNC($$$):\n    $$$\n    import $MODULE",
                    "description": "Duplicate import inside function",
                    "quality": "bad",
                    "weight": -2,
                    "language": "python"
                }
            ],
            "python_runtime_modification": [
                {
                    "id": "singleton-state-change",
                    "pattern": "$SINGLETON.$ATTR = $VALUE",
                    "description": "Runtime singleton state modification",
                    "quality": "neutral",
                    "weight": 0,
                    "language": "python",
                    "note": "Can be good for mode switching, bad if uncontrolled"
                },
                {
                    "id": "public-init-exposure",
                    "pattern": "def try_initialize_$TYPE(self): $$$",
                    "description": "Public initialization method for runtime config",
                    "quality": "neutral",
                    "weight": 0,
                    "language": "python"
                }
            ]
        }

    def _load_typescript_patterns(self) -> Dict[str, List[Dict[str, Any]]]:
        """TypeScript-specific patterns from catalog."""
        return {
            "typescript_async": [
                {
                    "id": "no-await-in-promise-all",
                    "pattern": "await $A",
                    "inside": "Promise.all($_)",
                    "description": "No await in Promise.all array",
                    "quality": "bad",
                    "weight": -4,
                    "language": "typescript",
                    "fix": "$A"
                },
                {
                    "id": "async-function-ts",
                    "pattern": "async function $FUNC($$$) { $$$ }",
                    "description": "Async function",
                    "quality": "good",
                    "weight": 2,
                    "language": "typescript"
                },
                {
                    "id": "async-arrow",
                    "pattern": "async ($$$) => { $$$ }",
                    "description": "Async arrow function",
                    "quality": "good",
                    "weight": 2,
                    "language": "typescript"
                }
            ],
            "typescript_console": [
                {
                    "id": "no-console-log",
                    "pattern": "console.log($$$)",
                    "description": "Console.log usage",
                    "quality": "bad",
                    "weight": -2,
                    "language": "typescript",
                    "fix": ""
                },
                {
                    "id": "no-console-debug",
                    "pattern": "console.debug($$$)",
                    "description": "Console.debug usage",
                    "quality": "bad",
                    "weight": -2,
                    "language": "typescript",
                    "fix": ""
                },
                {
                    "id": "console-error-in-catch",
                    "pattern": "console.error($$$)",
                    "inside": "catch ($_) { $$$ }",
                    "description": "Console.error in catch (OK)",
                    "quality": "neutral",
                    "weight": 0,
                    "language": "typescript"
                }
            ],
            "typescript_react": [
                {
                    "id": "useState-hook",
                    "pattern": "const [$STATE, $SETTER] = useState($$$)",
                    "description": "React useState hook",
                    "quality": "good",
                    "weight": 2,
                    "language": "typescript"
                },
                {
                    "id": "useEffect-hook",
                    "pattern": "useEffect(() => { $$$ }, $DEPS)",
                    "description": "React useEffect hook",
                    "quality": "neutral",
                    "weight": 1,
                    "language": "typescript"
                },
                {
                    "id": "useEffect-no-deps",
                    "pattern": "useEffect(() => { $$$ })",
                    "description": "useEffect without dependencies",
                    "quality": "bad",
                    "weight": -3,
                    "language": "typescript"
                }
            ],
            "typescript_imports": [
                {
                    "id": "barrel-import",
                    "pattern": "import { $$$ } from '$MODULE'",
                    "description": "Named import",
                    "quality": "neutral",
                    "weight": 0,
                    "language": "typescript"
                },
                {
                    "id": "default-import",
                    "pattern": "import $NAME from '$MODULE'",
                    "description": "Default import",
                    "quality": "neutral",
                    "weight": 0,
                    "language": "typescript"
                },
                {
                    "id": "import-all",
                    "pattern": "import * as $NAME from '$MODULE'",
                    "description": "Import all",
                    "quality": "neutral",
                    "weight": -1,
                    "language": "typescript"
                }
            ]
        }

    def _load_shell_patterns(self) -> Dict[str, List[Dict[str, Any]]]:
        """Shell script patterns."""
        return {
            "shell_env_handling": [
                {
                    "id": "unused-shell-var",
                    "pattern": "$VAR=\"$VALUE\"",
                    "description": "Assigned but never referenced variable",
                    "quality": "bad",
                    "weight": -2,
                    "language": "bash",
                    "note": "Check if variable is used later"
                },
                {
                    "id": "unsafe-var-check",
                    "pattern": "[ ! -z \"$VAR\" ]",
                    "description": "Unsafe variable check (breaks with set -u)",
                    "quality": "bad",
                    "weight": -3,
                    "language": "bash",
                    "fix": "[ -n \"${VAR:-}\" ]"
                },
                {
                    "id": "redundant-export",
                    "pattern": "export $VAR=\"$VAR\"",
                    "description": "Redundant export of same value",
                    "quality": "bad",
                    "weight": -2,
                    "language": "bash"
                },
                {
                    "id": "missing-safety-flags",
                    "pattern": "#!/bin/bash",
                    "description": "Missing safety flags",
                    "quality": "bad",
                    "weight": -3,
                    "language": "bash",
                    "note": "Add 'set -euo pipefail' after shebang"
                }
            ]
        }

    def _load_javascript_patterns(self) -> Dict[str, List[Dict[str, Any]]]:
        """JavaScript patterns (subset of TypeScript)."""
        return {
            "javascript_async": [
                {
                    "id": "callback-hell",
                    "pattern": "$FUNC($$$, function($$$) { $$$ })",
                    "description": "Callback pattern (consider promises)",
                    "quality": "bad",
                    "weight": -2,
                    "language": "javascript"
                },
                {
                    "id": "promise-then",
                    "pattern": "$PROMISE.then($$$)",
                    "description": "Promise then chain",
                    "quality": "neutral",
                    "weight": 0,
                    "language": "javascript"
                },
                {
                    "id": "async-await",
                    "pattern": "await $PROMISE",
                    "description": "Async/await usage",
                    "quality": "good",
                    "weight": 2,
                    "language": "javascript"
                }
            ],
            "javascript_var": [
                {
                    "id": "var-declaration",
                    "pattern": "var $VAR = $$$",
                    "description": "Var declaration (use const/let)",
                    "quality": "bad",
                    "weight": -3,
                    "language": "javascript"
                },
                {
                    "id": "const-declaration",
                    "pattern": "const $VAR = $$$",
                    "description": "Const declaration",
                    "quality": "good",
                    "weight": 2,
                    "language": "javascript"
                },
                {
                    "id": "let-declaration",
                    "pattern": "let $VAR = $$$",
                    "description": "Let declaration",
                    "quality": "good",
                    "weight": 1,
                    "language": "javascript"
                }
            ]
        }

    def get_all_patterns(self) -> List[Dict[str, Any]]:
        """Get all patterns as a flat list."""
        all_patterns = []
        for category, patterns in self.patterns.items():
            for pattern in patterns:
                # Avoid mutating source; create a copy
                item = dict(pattern)
                item['category'] = category
                all_patterns.append(item)
        return all_patterns

    def get_patterns_by_language(self, language: str) -> List[Dict[str, Any]]:
        """Get patterns for a specific language."""
        return [p for p in self.get_all_patterns() if p.get('language') == language]

    def get_good_patterns(self) -> List[Dict[str, Any]]:
        """Get only good quality patterns."""
        return [p for p in self.get_all_patterns() if p.get('quality') == 'good']

    def get_bad_patterns(self) -> List[Dict[str, Any]]:
        """Get only bad quality patterns (anti-patterns)."""
        return [p for p in self.get_all_patterns() if p.get('quality') == 'bad']

    def calculate_quality_score(self, matches: List[Dict], loc: int = 1000) -> float:
        """
        Calculate quality score using penalty-based approach.
        Issues dominate the score; good patterns provide minimal bonus.

        Args:
            matches: List of pattern matches with weight and count
            loc: Lines of code (for normalization)

        Returns:
            Score from 0.0 to 1.0
        """
        import math

        # Normalize to KLOC (thousands of lines)
        kloc = max(1.0, loc / 1000.0)

        # Separate issues (bad) from good patterns
        issues = [m for m in matches if m.get('quality') == 'bad']
        good_patterns = [m for m in matches if m.get('quality') == 'good']

        # Calculate severity-weighted issue density
        total_issues = 0
        for issue in issues:
            severity = abs(issue.get('weight', 1))  # Use weight as severity
            count = issue.get('count', 0)
            total_issues += severity * count

        issues_per_kloc = total_issues / kloc

        # Penalty calculation (logarithmic to avoid linear dominance)
        # Calibrated so 50 issues/KLOC = ~50% penalty
        penalty = min(0.7, 0.15 * math.log1p(issues_per_kloc))

        # Small bonus for good patterns (capped at 5%)
        good_score = 0
        if good_patterns:
            for pattern in good_patterns:
                weight = pattern.get('weight', 1)
                count = pattern.get('count', 0)
                # Cap contribution per pattern type
                normalized_count = min(count / kloc, 50)  # Max 50 per KLOC
                good_score += weight * normalized_count / 1000

        bonus = min(0.05, good_score)  # Cap at 5% bonus

        # Final score: start at 100%, subtract penalty, add small bonus
        score = max(0.0, min(1.0, 1.0 - penalty + bonus))

        return score

    def export_to_json(self, path: str):
        """Export registry to JSON file."""
        data = {
            'source': 'unified-ast-grep',
            'version': '2.0.0',
            'patterns': self.patterns,
            'stats': {
                'total_patterns': len(self.get_all_patterns()),
                'good_patterns': len(self.get_good_patterns()),
                'bad_patterns': len(self.get_bad_patterns()),
                'languages': list(set(p.get('language') for p in self.get_all_patterns())),
                'categories': list(self.patterns.keys())
            }
        }

        with open(path, 'w') as f:
            json.dump(data, f, indent=2)


# Singleton instance
_unified_registry = None

def get_unified_registry() -> UnifiedASTGrepRegistry:
    """Get or create the unified AST-GREP pattern registry."""
    global _unified_registry
    if _unified_registry is None:
        _unified_registry = UnifiedASTGrepRegistry()
    return _unified_registry


if __name__ == "__main__":
    # Test the unified registry
    registry = get_unified_registry()

    print("Unified AST-GREP Pattern Registry")
    print("=" * 60)

    all_patterns = registry.get_all_patterns()
    print(f"\nTotal patterns: {len(all_patterns)}")
    print(f"Good patterns: {len(registry.get_good_patterns())}")
    print(f"Bad patterns: {len(registry.get_bad_patterns())}")

    # Count by language
    languages = {}
    for pattern in all_patterns:
        lang = pattern.get('language', 'unknown')
        languages[lang] = languages.get(lang, 0) + 1

    print(f"\nPatterns by language:")
    for lang, count in languages.items():
        print(f"  - {lang}: {count} patterns")

    print(f"\nCategories ({len(registry.patterns)}):")
    for category in registry.patterns.keys():
        count = len(registry.patterns[category])
        print(f"  - {category}: {count} patterns")

    # Export to JSON
    export_path = Path(__file__).parent / "unified_registry.json"
    registry.export_to_json(export_path)
    print(f"\n✅ Exported unified registry to {export_path}")

    # Show sample patterns
    print("\nSample patterns:")
    for pattern in all_patterns[:5]:
        print(f"  - {pattern['id']} ({pattern['language']}): {pattern.get('pattern', 'N/A')[:40]}...")
        print(f"    Quality: {pattern['quality']}, Weight: {pattern['weight']}")