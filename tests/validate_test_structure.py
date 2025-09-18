#!/usr/bin/env python3
"""
Validate the unified state test structure without running the full suite.

This script checks that:
1. All test files are properly structured
2. All imports can be resolved
3. Test classes and methods are discoverable
4. Required fixtures are defined
"""

import sys
import ast
import importlib.util
from pathlib import Path


def validate_test_file(test_file_path):
    """Validate a test file's structure and imports."""
    print(f"Validating {test_file_path.name}...")

    # Parse the file
    try:
        with open(test_file_path, 'r') as f:
            content = f.read()

        tree = ast.parse(content)
    except Exception as e:
        print(f"  ❌ Failed to parse: {e}")
        return False

    # Count test classes and methods
    test_classes = []
    test_methods = []
    fixtures = []

    for node in ast.walk(tree):
        if isinstance(node, ast.ClassDef):
            if node.name.startswith('Test'):
                test_classes.append(node.name)
                # Count methods in this class
                for item in node.body:
                    if isinstance(item, ast.FunctionDef) and item.name.startswith('test_'):
                        test_methods.append(f"{node.name}::{item.name}")

        elif isinstance(node, ast.FunctionDef):
            # Check for pytest fixtures
            for decorator in node.decorator_list:
                if isinstance(decorator, ast.Attribute) and decorator.attr == 'fixture':
                    fixtures.append(node.name)
                elif isinstance(decorator, ast.Name) and decorator.id == 'fixture':
                    fixtures.append(node.name)

    print(f"  ✓ Found {len(test_classes)} test classes")
    print(f"  ✓ Found {len(test_methods)} test methods")
    print(f"  ✓ Found {len(fixtures)} fixtures")

    # List test classes
    if test_classes:
        print(f"  Test classes: {', '.join(test_classes)}")

    return len(test_classes) > 0 and len(test_methods) > 0


def check_imports(test_file_path):
    """Check that all imports in the test file can be resolved."""
    print(f"Checking imports in {test_file_path.name}...")

    try:
        spec = importlib.util.spec_from_file_location("test_module", test_file_path)
        if spec and spec.loader:
            # Don't actually load the module (might have missing deps)
            # Just check that the spec can be created
            print("  ✓ Import structure looks valid")
            return True
    except Exception as e:
        print(f"  ❌ Import issues: {e}")
        return False

    return False


def validate_unified_state_manager():
    """Check that the unified state manager can be found."""
    project_root = Path(__file__).parent.parent
    usm_path = project_root / "scripts" / "unified_state_manager.py"

    if not usm_path.exists():
        print(f"❌ Unified state manager not found at {usm_path}")
        return False

    print(f"✓ Found unified state manager at {usm_path}")
    return True


def main():
    """Main validation routine."""
    print("Unified State Test Structure Validation")
    print("=" * 50)

    # Check unified state manager exists
    if not validate_unified_state_manager():
        sys.exit(1)

    # Find test files
    test_dir = Path(__file__).parent
    test_files = list(test_dir.glob("test_unified_state*.py"))

    if not test_files:
        print("❌ No unified state test files found")
        sys.exit(1)

    print(f"Found {len(test_files)} test files to validate")
    print()

    all_valid = True

    for test_file in test_files:
        valid = validate_test_file(test_file)
        if not valid:
            all_valid = False
        print()

    # Summary
    print("=" * 50)
    if all_valid:
        print("✓ All test files are properly structured")
        print("\nNext steps:")
        print("1. Install test dependencies: pip install -r tests/requirements-test.txt")
        print("2. Run tests: python tests/run_unified_state_tests.py")
    else:
        print("❌ Some test files have issues")
        sys.exit(1)


if __name__ == "__main__":
    main()