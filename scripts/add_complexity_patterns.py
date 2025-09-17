#!/usr/bin/env python3
"""
Add logic flow complexity patterns to the unified registry.
These patterns detect complex control flow that indicates high cyclomatic complexity.
"""

import json
from pathlib import Path
from datetime import datetime

def add_complexity_patterns():
    """Add logic flow complexity patterns to the registry."""

    registry_path = Path(__file__).parent / "unified_registry.json"

    # Load existing registry
    with open(registry_path, 'r') as f:
        registry = json.load(f)

    # Define new complexity patterns for Python
    python_complexity_patterns = [
        {
            "id": "nested-if-depth-3",
            "pattern": "if $COND1:\n    $$$\n    if $COND2:\n        $$$\n        if $COND3:\n            $$$",
            "description": "Deeply nested if statements (3+ levels)",
            "quality": "bad",
            "weight": -4,
            "severity": "medium",
            "language": "python",
            "category": "python_complexity"
        },
        {
            "id": "complex-condition",
            "pattern": "if $A and $B and $C and $D",
            "description": "Complex conditional with 4+ conditions",
            "quality": "bad",
            "weight": -2,
            "severity": "low",
            "language": "python",
            "category": "python_complexity"
        },
        {
            "id": "nested-loops",
            "pattern": "for $VAR1 in $ITER1:\n    $$$\n    for $VAR2 in $ITER2:\n        $$$",
            "description": "Nested loops (performance risk)",
            "quality": "bad",
            "weight": -3,
            "severity": "medium",
            "language": "python",
            "category": "python_complexity"
        },
        {
            "id": "multiple-elif",
            "pattern": "if $COND1:\n    $$$\nelif $COND2:\n    $$$\nelif $COND3:\n    $$$\nelif $COND4:\n    $$$\nelif $COND5:\n    $$$",
            "description": "Many elif branches (5+)",
            "quality": "bad",
            "weight": -3,
            "severity": "medium",
            "language": "python",
            "category": "python_complexity"
        },
        {
            "id": "long-function",
            "pattern": "def $FUNC($$$):\n    $LINE1\n    $LINE2\n    $LINE3\n    $LINE4\n    $LINE5\n    $LINE6\n    $LINE7\n    $LINE8\n    $LINE9\n    $LINE10\n    $$$",
            "description": "Long function (10+ statements)",
            "quality": "bad",
            "weight": -2,
            "severity": "low",
            "language": "python",
            "category": "python_complexity"
        }
    ]

    # Define complexity patterns for TypeScript/JavaScript
    typescript_complexity_patterns = [
        {
            "id": "nested-if-depth-3-ts",
            "pattern": "if ($COND1) {\n  $$$\n  if ($COND2) {\n    $$$\n    if ($COND3) {\n      $$$\n    }\n  }\n}",
            "description": "Deeply nested if statements (3+ levels)",
            "quality": "bad",
            "weight": -4,
            "severity": "medium",
            "language": "typescript",
            "category": "typescript_complexity"
        },
        {
            "id": "complex-condition-ts",
            "pattern": "if ($A && $B && $C && $D)",
            "description": "Complex conditional with 4+ conditions",
            "quality": "bad",
            "weight": -2,
            "severity": "low",
            "language": "typescript",
            "category": "typescript_complexity"
        },
        {
            "id": "nested-loops-ts",
            "pattern": "for ($INIT1; $COND1; $INC1) {\n  $$$\n  for ($INIT2; $COND2; $INC2) {\n    $$$\n  }\n}",
            "description": "Nested loops (performance risk)",
            "quality": "bad",
            "weight": -3,
            "severity": "medium",
            "language": "typescript",
            "category": "typescript_complexity"
        },
        {
            "id": "callback-hell",
            "pattern": "$FUNC1(($ARG1) => {\n  $$$\n  $FUNC2(($ARG2) => {\n    $$$\n    $FUNC3(($ARG3) => {\n      $$$\n    })\n  })\n})",
            "description": "Callback hell (3+ levels)",
            "quality": "bad",
            "weight": -4,
            "severity": "high",
            "language": "typescript",
            "category": "typescript_complexity"
        },
        {
            "id": "switch-many-cases",
            "pattern": "switch ($VAR) {\n  case $CASE1:\n    $$$\n  case $CASE2:\n    $$$\n  case $CASE3:\n    $$$\n  case $CASE4:\n    $$$\n  case $CASE5:\n    $$$\n  case $CASE6:\n    $$$\n  case $CASE7:\n    $$$\n}",
            "description": "Switch with many cases (7+)",
            "quality": "bad",
            "weight": -3,
            "severity": "medium",
            "language": "typescript",
            "category": "typescript_complexity"
        }
    ]

    # Add JavaScript versions (same patterns, different language tag)
    javascript_complexity_patterns = [
        {**p, "language": "javascript", "category": "javascript_complexity"}
        for p in typescript_complexity_patterns
    ]
    for p in javascript_complexity_patterns:
        p["id"] = p["id"].replace("-ts", "-js")

    # Add new patterns to registry
    if "python_complexity" not in registry["patterns"]:
        registry["patterns"]["python_complexity"] = []
    if "typescript_complexity" not in registry["patterns"]:
        registry["patterns"]["typescript_complexity"] = []
    if "javascript_complexity" not in registry["patterns"]:
        registry["patterns"]["javascript_complexity"] = []

    registry["patterns"]["python_complexity"].extend(python_complexity_patterns)
    registry["patterns"]["typescript_complexity"].extend(typescript_complexity_patterns)
    registry["patterns"]["javascript_complexity"].extend(javascript_complexity_patterns)

    # Update stats
    registry["stats"]["total_patterns"] = sum(
        len(patterns) for patterns in registry["patterns"].values()
    )

    # Add new categories if not present
    for cat in ["python_complexity", "typescript_complexity", "javascript_complexity"]:
        if cat not in registry["stats"]["categories"]:
            registry["stats"]["categories"].append(cat)

    # Update timestamp
    registry["timestamp"] = datetime.now().isoformat()

    # Save updated registry
    with open(registry_path, 'w') as f:
        json.dump(registry, f, indent=2)

    print(f"✅ Added {len(python_complexity_patterns)} Python complexity patterns")
    print(f"✅ Added {len(typescript_complexity_patterns)} TypeScript complexity patterns")
    print(f"✅ Added {len(javascript_complexity_patterns)} JavaScript complexity patterns")
    print(f"📊 Total patterns now: {registry['stats']['total_patterns']}")

if __name__ == "__main__":
    add_complexity_patterns()