#!/usr/bin/env python3
"""
Merge CodeRabbit-identified patterns into the unified AST-GREP registry.
This script carefully adds new patterns without duplicating existing ones.
"""

import json
from pathlib import Path
from datetime import datetime
from typing import Dict, List, Any

def load_registry(file_path: str) -> Dict:
    """Load the current registry."""
    with open(file_path, 'r') as f:
        return json.load(f)

def save_registry(registry: Dict, file_path: str):
    """Save the updated registry."""
    with open(file_path, 'w') as f:
        json.dump(registry, f, indent=2)

def pattern_exists(registry: Dict, pattern_id: str) -> bool:
    """Check if a pattern ID already exists in any category."""
    for category, patterns in registry.get('patterns', {}).items():
        if isinstance(patterns, list):
            for p in patterns:
                if p.get('id') == pattern_id:
                    return True
    return False

def convert_to_registry_format(pattern_data: Dict, pattern_id: str) -> Dict:
    """Convert CodeRabbit pattern format to registry format."""
    return {
        "id": pattern_id,
        "pattern": pattern_data["pattern"],
        "description": pattern_data["description"],
        "quality": pattern_data["quality"],
        "weight": pattern_data["weight"],
        "language": "python",
        "severity": pattern_data.get("severity"),
        "fix": pattern_data.get("fix"),
        "context": pattern_data.get("context")
    }

def clean_pattern_dict(pattern: Dict) -> Dict:
    """Remove None values from pattern dict."""
    return {k: v for k, v in pattern.items() if v is not None}

def main():
    # Load CodeRabbit patterns
    from coderabbit_identified_patterns import (
        SECURITY_PATTERNS, EXCEPTION_HANDLING_PATTERNS, IMPORT_PATTERNS,
        TYPE_SAFETY_PATTERNS, STRING_PATTERNS, SECURITY_HASHING_PATTERNS,
        PATH_PATTERNS, UNUSED_CODE_PATTERNS, PSUTIL_PATTERNS, GOOD_PATTERNS
    )

    # Load current registry
    registry_path = "unified_registry.json"
    registry = load_registry(registry_path)

    # Initialize new categories if they don't exist
    if 'patterns' not in registry:
        registry['patterns'] = {}

    # Categories to update/create
    categories_map = {
        'python_security': SECURITY_PATTERNS,
        'python_exceptions': EXCEPTION_HANDLING_PATTERNS,
        'python_imports': IMPORT_PATTERNS,
        'python_type_safety': TYPE_SAFETY_PATTERNS,
        'python_strings': STRING_PATTERNS,
        'python_hashing': SECURITY_HASHING_PATTERNS,
        'python_paths': PATH_PATTERNS,
        'python_unused': UNUSED_CODE_PATTERNS,
        'python_psutil': PSUTIL_PATTERNS,
        'python_best_practices': GOOD_PATTERNS
    }

    # Track statistics
    stats = {
        'added': 0,
        'skipped': 0,
        'updated_categories': []
    }

    # Process each category
    for category_name, patterns_dict in categories_map.items():
        # Initialize category if it doesn't exist
        if category_name not in registry['patterns']:
            registry['patterns'][category_name] = []
            print(f"Created new category: {category_name}")

        category_patterns = registry['patterns'][category_name]

        # Add patterns to category
        for pattern_id, pattern_data in patterns_dict.items():
            if not pattern_exists(registry, pattern_id):
                new_pattern = convert_to_registry_format(pattern_data, pattern_id)
                new_pattern = clean_pattern_dict(new_pattern)
                category_patterns.append(new_pattern)
                stats['added'] += 1
                print(f"  Added: {pattern_id}")
            else:
                stats['skipped'] += 1
                print(f"  Skipped (exists): {pattern_id}")

        if patterns_dict:
            stats['updated_categories'].append(category_name)

    # Update metadata
    registry['version'] = "4.0.0"  # Bump version
    registry['timestamp'] = datetime.now().isoformat()
    registry['source'] = "unified-ast-grep-with-coderabbit-patterns"

    # Add statistics to registry
    registry['statistics'] = {
        'total_patterns': sum(len(patterns) for patterns in registry['patterns'].values() if isinstance(patterns, list)),
        'categories': len(registry['patterns']),
        'last_coderabbit_update': datetime.now().isoformat(),
        'coderabbit_patterns_added': stats['added']
    }

    # Save updated registry
    save_registry(registry, registry_path)

    # Print summary
    print("\n" + "=" * 50)
    print("MERGE COMPLETE")
    print("=" * 50)
    print(f"Patterns added: {stats['added']}")
    print(f"Patterns skipped: {stats['skipped']}")
    print(f"Categories updated: {', '.join(stats['updated_categories'])}")
    print(f"Total patterns now: {registry['statistics']['total_patterns']}")
    print(f"Total categories: {registry['statistics']['categories']}")

    # Create backup
    backup_path = f"unified_registry_backup_{datetime.now().strftime('%Y%m%d_%H%M%S')}.json"
    save_registry(registry, backup_path)
    print(f"\nBackup saved to: {backup_path}")

if __name__ == "__main__":
    main()