#!/usr/bin/env python3
"""
Show an overview of the unified state test suite structure.

This provides a hierarchical view of all test categories and individual tests.
"""

import ast
from pathlib import Path


def analyze_test_file(test_file_path):
    """Analyze test file and return structured information."""
    with open(test_file_path, 'r') as f:
        content = f.read()

    tree = ast.parse(content)

    classes = {}

    for node in ast.walk(tree):
        if isinstance(node, ast.ClassDef) and node.name.startswith('Test'):
            class_name = node.name
            methods = []

            # Get class docstring
            class_doc = ast.get_docstring(node) or "No description"
            class_doc = class_doc.split('\n')[0]  # First line only

            # Find test methods
            for item in node.body:
                if isinstance(item, ast.FunctionDef) and item.name.startswith('test_'):
                    method_doc = ast.get_docstring(item) or "No description"
                    method_doc = method_doc.split('\n')[0]  # First line only
                    methods.append({
                        'name': item.name,
                        'doc': method_doc
                    })

            classes[class_name] = {
                'doc': class_doc,
                'methods': methods
            }

    return classes


def show_overview():
    """Show complete test suite overview."""
    print("Unified State Management Test Suite Overview")
    print("=" * 80)

    test_dir = Path(__file__).parent
    test_file = test_dir / "test_unified_state.py"

    if not test_file.exists():
        print("❌ Test file not found")
        return

    classes = analyze_test_file(test_file)

    # Category mapping for better organization
    category_order = [
        "TestMigration",
        "TestConcurrency",
        "TestPerformance",
        "TestSecurity",
        "TestCrossPlatform",
        "TestIntegration",
        "TestEdgeCases"
    ]

    total_tests = 0

    for class_name in category_order:
        if class_name in classes:
            class_info = classes[class_name]
            category_name = class_name.replace('Test', '')

            print(f"\n{category_name} Tests ({len(class_info['methods'])} tests)")
            print("-" * 60)
            print(f"Description: {class_info['doc']}")
            print()

            for i, method in enumerate(class_info['methods'], 1):
                test_name = method['name'].replace('test_', '').replace('_', ' ').title()
                print(f"  {i:2d}. {test_name}")
                if method['doc'] != "No description":
                    print(f"      {method['doc']}")

            total_tests += len(class_info['methods'])

    # Show summary
    print("\n" + "=" * 80)
    print("SUMMARY")
    print("=" * 80)
    print(f"Total Test Categories: {len(category_order)}")
    print(f"Total Test Methods: {total_tests}")
    print()

    # Show category breakdown
    print("Test Distribution:")
    for class_name in category_order:
        if class_name in classes:
            category_name = class_name.replace('Test', '')
            count = len(classes[class_name]['methods'])
            bar = "█" * (count // 2)
            print(f"  {category_name:15} {count:2d} tests {bar}")

    print()
    print("Key Test Areas Covered:")
    print("  ✓ Migration from v3.x to v5.0 state format")
    print("  ✓ Concurrent access with file locking")
    print("  ✓ Performance with large datasets (1000+ files)")
    print("  ✓ Security validation and path traversal protection")
    print("  ✓ Cross-platform compatibility (Windows/Unix/Docker)")
    print("  ✓ Integration with batch/streaming importers")
    print("  ✓ Error recovery and edge case handling")

    print("\nRunning the Tests:")
    print("  python tests/run_unified_state_tests.py --scenarios")


if __name__ == "__main__":
    show_overview()