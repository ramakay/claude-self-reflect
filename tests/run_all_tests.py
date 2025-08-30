#!/usr/bin/env python3
"""
Comprehensive Test Runner for Claude Self-Reflect
Runs all test suites and provides detailed reporting.
"""

import os
import sys
import json
import time
import asyncio
import argparse
import subprocess
from pathlib import Path
from datetime import datetime
from typing import Dict, List, Any, Tuple

# Test categories and their files
TEST_SUITES = {
    "mcp_tools": {
        "file": "test_mcp_tools_comprehensive.py",
        "description": "MCP tool integration tests",
        "async": True
    },
    "memory_decay": {
        "file": "test_memory_decay.py",
        "description": "Memory decay feature tests",
        "async": False
    },
    "multi_project": {
        "file": "test_multi_project.py",
        "description": "Multi-project support tests",
        "async": True
    },
    "embedding_models": {
        "file": "test_embedding_models.py",
        "description": "Embedding model switching tests",
        "async": True
    },
    "delta_metadata": {
        "file": "test_delta_metadata.py",
        "description": "Delta metadata update tests",
        "async": True
    },
    "performance": {
        "file": "test_performance_load.py",
        "description": "Performance and load tests",
        "async": True
    },
    "data_integrity": {
        "file": "test_data_integrity.py",
        "description": "Data integrity tests",
        "async": True
    },
    "recovery": {
        "file": "test_recovery_scenarios.py",
        "description": "Recovery scenario tests",
        "async": True
    },
    "security": {
        "file": "test_security.py",
        "description": "Security validation tests",
        "async": False
    },
    # Existing test files
    "e2e_import": {
        "file": "../scripts/test-e2e-import.py",
        "description": "End-to-end import test",
        "async": True
    },
    "mcp_search": {
        "file": "../scripts/test-mcp-search.py",
        "description": "MCP search functionality",
        "async": True
    },
    "search_functionality": {
        "file": "../scripts/test-search-functionality.py",
        "description": "Search functionality tests",
        "async": True
    },
    "streaming_importer": {
        "file": "../scripts/test-streaming-importer-e2e.py",
        "description": "Streaming importer E2E test",
        "async": True
    },
    "streaming_watcher": {
        "file": "test_streaming_watcher.py",
        "description": "Streaming watcher comprehensive tests",
        "async": True
    }
}

class TestRunner:
    """Comprehensive test runner with reporting."""
    
    def __init__(self, verbose: bool = False, categories: List[str] = None):
        self.verbose = verbose
        self.categories = categories or list(TEST_SUITES.keys())
        self.results = {}
        self.start_time = None
        self.test_dir = Path(__file__).parent
    
    def check_prerequisites(self) -> Tuple[bool, List[str]]:
        """Check if all prerequisites are met."""
        issues = []
        
        # Check Docker
        try:
            result = subprocess.run(
                ["docker", "compose", "ps"],
                capture_output=True,
                text=True,
                check=False
            )
            if result.returncode != 0:
                issues.append("Docker Compose is not running")
            elif "qdrant" not in result.stdout:
                issues.append("Qdrant container is not running")
        except FileNotFoundError:
            issues.append("Docker Compose not found")
        
        # Check Python dependencies
        required_packages = [
            "qdrant-client",
            "fastembed",
            "voyageai",
            "fastmcp"
        ]
        
        for package in required_packages:
            try:
                __import__(package.replace("-", "_"))
            except ImportError:
                issues.append(f"Python package '{package}' not installed")
        
        # Check MCP server
        mcp_server_path = self.test_dir.parent / "mcp-server" / "src" / "server.py"
        if not mcp_server_path.exists():
            issues.append("MCP server not found")
        
        return len(issues) == 0, issues
    
    async def run_test_suite(self, name: str, config: Dict) -> Dict:
        """Run a single test suite."""
        test_file = self.test_dir / config["file"]
        
        # Handle relative paths for existing tests
        if not test_file.exists() and config["file"].startswith("../"):
            test_file = self.test_dir / Path(config["file"])
        
        if not test_file.exists():
            return {
                "name": name,
                "status": "SKIP",
                "error": f"Test file not found: {test_file}",
                "duration": 0
            }
        
        print(f"\n{'='*60}")
        print(f"📋 Running: {config['description']}")
        print(f"   File: {test_file.name}")
        print(f"{'='*60}")
        
        start = time.time()
        
        try:
            # Run the test
            result = subprocess.run(
                [sys.executable, str(test_file)],
                capture_output=True,
                text=True,
                timeout=300  # 5 minute timeout
            )
            
            duration = time.time() - start
            
            # Parse output for results
            output = result.stdout
            if self.verbose:
                print(output)
            
            # Check for pass/fail indicators
            passed = result.returncode == 0
            
            # Extract test counts from output
            test_counts = self.parse_test_output(output)
            
            return {
                "name": name,
                "status": "PASS" if passed else "FAIL",
                "duration": duration,
                "counts": test_counts,
                "output": output if not passed else None
            }
            
        except subprocess.TimeoutExpired:
            return {
                "name": name,
                "status": "TIMEOUT",
                "error": "Test exceeded 5 minute timeout",
                "duration": 300
            }
        except Exception as e:
            return {
                "name": name,
                "status": "ERROR",
                "error": str(e),
                "duration": time.time() - start
            }
    
    def parse_test_output(self, output: str) -> Dict:
        """Parse test output for pass/fail counts."""
        counts = {
            "passed": 0,
            "failed": 0,
            "skipped": 0,
            "total": 0
        }
        
        # Look for common patterns
        lines = output.split("\n")
        for line in lines:
            line_lower = line.lower()
            if "passed:" in line_lower or "pass:" in line_lower:
                # Try to extract number
                parts = line.split()
                for i, part in enumerate(parts):
                    if "pass" in part.lower() and i + 1 < len(parts):
                        try:
                            counts["passed"] = int(parts[i + 1].strip(":,"))
                        except ValueError:
                            pass
            
            if "failed:" in line_lower or "fail:" in line_lower:
                parts = line.split()
                for i, part in enumerate(parts):
                    if "fail" in part.lower() and i + 1 < len(parts):
                        try:
                            counts["failed"] = int(parts[i + 1].strip(":,"))
                        except ValueError:
                            pass
        
        counts["total"] = counts["passed"] + counts["failed"] + counts["skipped"]
        return counts
    
    async def run_all_tests(self):
        """Run all selected test suites."""
        self.start_time = time.time()
        
        print("🚀 Claude Self-Reflect Comprehensive Test Suite")
        print("="*60)
        print(f"📅 Started: {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}")
        print(f"📋 Categories: {', '.join(self.categories)}")
        
        # Check prerequisites
        ready, issues = self.check_prerequisites()
        if not ready:
            print("\n⚠️  Prerequisites not met:")
            for issue in issues:
                print(f"  - {issue}")
            print("\nPlease resolve these issues before running tests.")
            return False
        
        print("\n✅ All prerequisites met")
        
        # Run each test suite
        for category in self.categories:
            if category not in TEST_SUITES:
                print(f"\n⚠️  Unknown category: {category}")
                continue
            
            config = TEST_SUITES[category]
            result = await self.run_test_suite(category, config)
            self.results[category] = result
        
        # Print summary
        self.print_summary()
        
        # Save results to file
        self.save_results()
        
        # Return overall success
        return all(
            r["status"] == "PASS" 
            for r in self.results.values() 
            if r["status"] != "SKIP"
        )
    
    def print_summary(self):
        """Print test results summary."""
        duration = time.time() - self.start_time
        
        print("\n" + "="*60)
        print("📊 TEST RESULTS SUMMARY")
        print("="*60)
        
        # Count results by status
        status_counts = {}
        for result in self.results.values():
            status = result["status"]
            status_counts[status] = status_counts.get(status, 0) + 1
        
        # Print status summary
        total_tests = len(self.results)
        print(f"\n📋 Test Suites Run: {total_tests}")
        
        if "PASS" in status_counts:
            print(f"✅ Passed: {status_counts['PASS']}")
        if "FAIL" in status_counts:
            print(f"❌ Failed: {status_counts['FAIL']}")
        if "SKIP" in status_counts:
            print(f"⚠️  Skipped: {status_counts['SKIP']}")
        if "ERROR" in status_counts:
            print(f"🔥 Errors: {status_counts['ERROR']}")
        if "TIMEOUT" in status_counts:
            print(f"⏱️  Timeouts: {status_counts['TIMEOUT']}")
        
        print(f"\n⏱️  Total Duration: {duration:.1f}s")
        
        # Print failed tests
        failed = [
            (name, result) 
            for name, result in self.results.items() 
            if result["status"] in ["FAIL", "ERROR", "TIMEOUT"]
        ]
        
        if failed:
            print("\n" + "="*60)
            print("❌ FAILED TESTS:")
            print("="*60)
            for name, result in failed:
                print(f"\n📋 {name}: {result['status']}")
                if "error" in result:
                    print(f"   Error: {result['error']}")
                if "output" in result and result["output"]:
                    print("   Output (last 10 lines):")
                    lines = result["output"].split("\n")[-10:]
                    for line in lines:
                        if line.strip():
                            print(f"     {line}")
        
        # Print performance metrics
        print("\n" + "="*60)
        print("⚡ PERFORMANCE METRICS:")
        print("="*60)
        
        for name, result in sorted(
            self.results.items(),
            key=lambda x: x[1].get("duration", 0),
            reverse=True
        )[:5]:
            if "duration" in result:
                print(f"  {name}: {result['duration']:.1f}s")
    
    def save_results(self):
        """Save test results to JSON file."""
        results_file = self.test_dir / "test_results.json"
        
        results_data = {
            "timestamp": datetime.now().isoformat(),
            "duration": time.time() - self.start_time,
            "categories": self.categories,
            "results": self.results
        }
        
        with open(results_file, "w") as f:
            json.dump(results_data, f, indent=2, default=str)
        
        print(f"\n💾 Results saved to: {results_file}")
    
    def generate_report(self):
        """Generate HTML test report."""
        # This could be expanded to create a nice HTML report
        pass

async def main():
    """Main entry point."""
    parser = argparse.ArgumentParser(
        description="Run Claude Self-Reflect comprehensive test suite"
    )
    parser.add_argument(
        "-v", "--verbose",
        action="store_true",
        help="Show verbose output"
    )
    parser.add_argument(
        "-c", "--categories",
        nargs="+",
        choices=list(TEST_SUITES.keys()) + ["all"],
        default=["all"],
        help="Test categories to run"
    )
    parser.add_argument(
        "--list",
        action="store_true",
        help="List available test categories"
    )
    
    args = parser.parse_args()
    
    if args.list:
        print("Available test categories:")
        for name, config in TEST_SUITES.items():
            print(f"  {name:20} - {config['description']}")
        return
    
    # Determine categories to run
    if "all" in args.categories:
        categories = list(TEST_SUITES.keys())
    else:
        categories = args.categories
    
    # Run tests
    runner = TestRunner(verbose=args.verbose, categories=categories)
    success = await runner.run_all_tests()
    
    sys.exit(0 if success else 1)

if __name__ == "__main__":
    asyncio.run(main())