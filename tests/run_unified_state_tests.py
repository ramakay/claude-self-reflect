#!/usr/bin/env python3
"""
Test runner for unified state management tests.

This script provides convenient ways to run different categories of tests
and generate reports for the unified state management system.

Usage:
    python tests/run_unified_state_tests.py                    # Run all tests
    python tests/run_unified_state_tests.py migration          # Run migration tests only
    python tests/run_unified_state_tests.py performance        # Run performance tests only
    python tests/run_unified_state_tests.py --quick            # Run quick tests only
    python tests/run_unified_state_tests.py --report           # Generate detailed report
"""

import sys
import argparse
import subprocess
import time
from pathlib import Path
from datetime import datetime


def run_tests(test_category=None, quick=False, report=False, verbose=False):
    """
    Run specified category of tests.

    Args:
        test_category: Specific test category to run (migration, concurrency, etc.)
        quick: Run only quick tests (skip slow ones)
        report: Generate detailed test report
        verbose: Verbose output
    """
    project_root = Path(__file__).parent.parent
    test_file = project_root / "tests" / "test_unified_state.py"

    # Build pytest command
    cmd = ["python", "-m", "pytest", str(test_file)]

    if verbose:
        cmd.append("-v")
    else:
        cmd.append("-q")

    if quick:
        cmd.extend(["-m", "not slow"])

    if test_category:
        # Map categories to test class names
        category_map = {
            "migration": "TestMigration",
            "concurrency": "TestConcurrency",
            "performance": "TestPerformance",
            "security": "TestSecurity",
            "cross-platform": "TestCrossPlatform",
            "integration": "TestIntegration",
            "edge-cases": "TestEdgeCases"
        }

        if test_category in category_map:
            cmd.extend(["-k", category_map[test_category]])
        else:
            print(f"Unknown test category: {test_category}")
            print(f"Available categories: {', '.join(category_map.keys())}")
            return False

    if report:
        # Add reporting options
        timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
        report_dir = project_root / "tests" / "reports" / f"unified_state_{timestamp}"
        report_dir.mkdir(parents=True, exist_ok=True)

        cmd.extend([
            "--html", str(report_dir / "report.html"),
            "--self-contained-html",
            "--junitxml", str(report_dir / "junit.xml"),
            "--durations=20"
        ])

        if sys.version_info >= (3, 8):
            try:
                import pytest_cov
                cmd.extend([
                    "--cov=scripts.unified_state_manager",
                    "--cov-report=html:" + str(report_dir / "coverage"),
                    "--cov-report=term"
                ])
            except ImportError:
                pass

    print(f"Running command: {' '.join(cmd)}")
    print("-" * 60)

    start_time = time.time()
    result = subprocess.run(cmd, cwd=project_root)
    duration = time.time() - start_time

    print("-" * 60)
    print(f"Tests completed in {duration:.2f} seconds")

    if report and result.returncode == 0:
        print(f"Test report generated in: {report_dir}")

    return result.returncode == 0


def check_dependencies():
    """Check that required dependencies are installed."""
    required_packages = [
        "pytest",
        "psutil",
        "mock"
    ]

    missing = []
    for package in required_packages:
        try:
            __import__(package)
        except ImportError:
            missing.append(package)

    if missing:
        print("Missing required packages:")
        for package in missing:
            print(f"  - {package}")
        print("\nInstall with:")
        print(f"  pip install {' '.join(missing)}")
        print("\nOr install all test requirements:")
        print("  pip install -r tests/requirements-test.txt")
        return False

    return True


def run_specific_test_scenarios():
    """Run specific test scenarios and report results."""
    scenarios = [
        ("Migration Tests", "migration"),
        ("Concurrency Tests", "concurrency"),
        ("Performance Tests", "performance"),
        ("Security Tests", "security"),
        ("Integration Tests", "integration"),
        ("Edge Cases", "edge-cases")
    ]

    results = {}

    print("Running Unified State Management Test Suite")
    print("=" * 60)

    for name, category in scenarios:
        print(f"\n{name}:")
        print("-" * 40)

        success = run_tests(test_category=category, verbose=False)
        results[name] = success

        status = "PASSED" if success else "FAILED"
        print(f"{name}: {status}")

    print("\n" + "=" * 60)
    print("SUMMARY:")
    print("=" * 60)

    passed = sum(1 for success in results.values() if success)
    total = len(results)

    for name, success in results.items():
        status = "✓" if success else "✗"
        print(f"{status} {name}")

    print(f"\nOverall: {passed}/{total} test categories passed")

    return passed == total


def main():
    """Main entry point."""
    parser = argparse.ArgumentParser(
        description="Run unified state management tests",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Test Categories:
  migration      - Migration from old state formats
  concurrency    - Multi-threading and locking tests
  performance    - Speed and memory usage tests
  security       - Path traversal and input validation
  cross-platform - Windows/Unix compatibility
  integration    - Integration with other components
  edge-cases     - Error conditions and edge cases

Examples:
  %(prog)s                           # Run all tests
  %(prog)s migration                 # Run migration tests only
  %(prog)s --quick                   # Run quick tests only
  %(prog)s --report                  # Generate detailed report
  %(prog)s performance --verbose     # Run performance tests with verbose output
  %(prog)s --scenarios               # Run all scenarios with summary
"""
    )

    parser.add_argument(
        "category",
        nargs="?",
        help="Test category to run (migration, concurrency, performance, etc.)"
    )

    parser.add_argument(
        "--quick",
        action="store_true",
        help="Run only quick tests (skip slow ones)"
    )

    parser.add_argument(
        "--report",
        action="store_true",
        help="Generate detailed test report with HTML output"
    )

    parser.add_argument(
        "--verbose", "-v",
        action="store_true",
        help="Verbose output"
    )

    parser.add_argument(
        "--scenarios",
        action="store_true",
        help="Run all test scenarios and show summary"
    )

    parser.add_argument(
        "--check-deps",
        action="store_true",
        help="Check dependencies only"
    )

    args = parser.parse_args()

    if args.check_deps:
        success = check_dependencies()
        sys.exit(0 if success else 1)

    # Check dependencies first
    if not check_dependencies():
        sys.exit(1)

    # Run tests
    if args.scenarios:
        success = run_specific_test_scenarios()
    else:
        success = run_tests(
            test_category=args.category,
            quick=args.quick,
            report=args.report,
            verbose=args.verbose
        )

    sys.exit(0 if success else 1)


if __name__ == "__main__":
    main()