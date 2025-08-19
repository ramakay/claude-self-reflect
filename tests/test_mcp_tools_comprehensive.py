#!/usr/bin/env python3
"""
Comprehensive MCP Tool Integration Tests
Tests all MCP tools with various parameters and edge cases.
"""

import os
import sys
import json
import asyncio
import tempfile
from pathlib import Path
from datetime import datetime, timedelta
from typing import Dict, List, Any

# Add parent directory to path
sys.path.insert(0, str(Path(__file__).parent.parent))
from mcp_server.src.server import (
    reflect_on_past,
    store_reflection,
    quick_search,
    search_summary,
    get_more_results,
    search_by_file,
    search_by_concept,
    SearchParams,
    StoreReflectionParams,
    FileSearchParams,
    ConceptSearchParams
)

class TestMCPTools:
    """Comprehensive test suite for MCP tools."""
    
    def __init__(self):
        self.test_results = []
        self.test_project = "test-project"
    
    async def setup(self):
        """Setup test environment."""
        print("🔧 Setting up test environment...")
        # Store some test reflections for searching
        test_reflections = [
            {
                "content": "Testing MCP reflection storage with Python code implementation",
                "tags": ["testing", "python", "mcp"]
            },
            {
                "content": "Security vulnerability found in authentication flow",
                "tags": ["security", "bug", "authentication"]
            },
            {
                "content": "Performance optimization using caching strategy",
                "tags": ["performance", "optimization", "cache"]
            }
        ]
        
        for reflection in test_reflections:
            try:
                await store_reflection(StoreReflectionParams(
                    content=reflection["content"],
                    tags=reflection["tags"]
                ))
            except Exception as e:
                print(f"Setup warning: {e}")
    
    async def test_reflect_on_past(self):
        """Test reflect_on_past with various parameters."""
        test_name = "reflect_on_past"
        print(f"\n📝 Testing {test_name}...")
        
        test_cases = [
            # Basic search
            {
                "params": {"query": "testing MCP", "limit": 5},
                "description": "Basic search with limit"
            },
            # Brief mode
            {
                "params": {"query": "security", "brief": True, "limit": 3},
                "description": "Brief mode search"
            },
            # With min_score threshold
            {
                "params": {"query": "performance", "min_score": 0.8},
                "description": "High similarity threshold"
            },
            # Cross-project search
            {
                "params": {"query": "authentication", "project": "all"},
                "description": "Search across all projects"
            },
            # With time decay
            {
                "params": {"query": "optimization", "use_decay": 1},
                "description": "Search with time decay enabled"
            },
            # Markdown format
            {
                "params": {"query": "cache", "response_format": "markdown"},
                "description": "Markdown response format"
            },
            # Include raw data
            {
                "params": {"query": "Python", "include_raw": True, "limit": 2},
                "description": "Include raw Qdrant payload"
            }
        ]
        
        for i, test_case in enumerate(test_cases, 1):
            try:
                print(f"  [{i}/{len(test_cases)}] {test_case['description']}...")
                result = await reflect_on_past(SearchParams(**test_case["params"]))
                
                # Validate response structure
                assert result is not None, "Result should not be None"
                
                if test_case["params"].get("brief"):
                    assert "RESULTS:" in str(result), "Brief mode should have RESULTS"
                
                if test_case["params"].get("response_format") == "markdown":
                    assert "##" in str(result) or "#" in str(result), "Markdown should have headers"
                
                self.test_results.append({
                    "test": f"{test_name}_{i}",
                    "status": "✅ PASS",
                    "description": test_case["description"]
                })
                
            except Exception as e:
                self.test_results.append({
                    "test": f"{test_name}_{i}",
                    "status": "❌ FAIL",
                    "error": str(e),
                    "description": test_case["description"]
                })
    
    async def test_store_reflection(self):
        """Test store_reflection functionality."""
        test_name = "store_reflection"
        print(f"\n📝 Testing {test_name}...")
        
        test_cases = [
            # Basic storage
            {
                "content": "Test reflection with basic content",
                "tags": ["test", "basic"]
            },
            # Long content
            {
                "content": "A" * 5000,  # 5000 character reflection
                "tags": ["long", "stress-test"]
            },
            # Unicode and special characters
            {
                "content": "Testing with émojis 🎉 and special chars: <>&\"'",
                "tags": ["unicode", "special-chars"]
            },
            # No tags
            {
                "content": "Reflection without tags",
                "tags": []
            },
            # Many tags
            {
                "content": "Reflection with many tags",
                "tags": [f"tag{i}" for i in range(20)]
            }
        ]
        
        for i, test_case in enumerate(test_cases, 1):
            try:
                print(f"  [{i}/{len(test_cases)}] Storing: {test_case['content'][:50]}...")
                result = await store_reflection(StoreReflectionParams(
                    content=test_case["content"],
                    tags=test_case["tags"]
                ))
                
                assert "stored successfully" in str(result).lower(), "Should confirm storage"
                
                self.test_results.append({
                    "test": f"{test_name}_{i}",
                    "status": "✅ PASS"
                })
                
            except Exception as e:
                self.test_results.append({
                    "test": f"{test_name}_{i}",
                    "status": "❌ FAIL",
                    "error": str(e)
                })
    
    async def test_quick_search(self):
        """Test quick_search functionality."""
        test_name = "quick_search"
        print(f"\n📝 Testing {test_name}...")
        
        test_cases = [
            {"query": "testing", "min_score": 0.5},
            {"query": "nonexistent-query-xyz", "min_score": 0.9},
            {"query": "security vulnerability", "project": self.test_project}
        ]
        
        for i, params in enumerate(test_cases, 1):
            try:
                print(f"  [{i}/{len(test_cases)}] Quick search: {params['query']}...")
                result = await quick_search(**params)
                
                # Should return count and top result only
                assert "count" in str(result).lower() or "result" in str(result).lower()
                
                self.test_results.append({
                    "test": f"{test_name}_{i}",
                    "status": "✅ PASS"
                })
                
            except Exception as e:
                self.test_results.append({
                    "test": f"{test_name}_{i}",
                    "status": "❌ FAIL",
                    "error": str(e)
                })
    
    async def test_search_summary(self):
        """Test search_summary functionality."""
        test_name = "search_summary"
        print(f"\n📝 Testing {test_name}...")
        
        try:
            result = await search_summary(
                query="testing MCP",
                project=None
            )
            
            # Should provide aggregated insights
            assert result is not None
            self.test_results.append({
                "test": test_name,
                "status": "✅ PASS"
            })
            
        except Exception as e:
            self.test_results.append({
                "test": test_name,
                "status": "❌ FAIL",
                "error": str(e)
            })
    
    async def test_get_more_results(self):
        """Test pagination with get_more_results."""
        test_name = "get_more_results"
        print(f"\n📝 Testing {test_name}...")
        
        try:
            # First get initial results
            initial = await reflect_on_past(SearchParams(
                query="test",
                limit=2
            ))
            
            # Then get more
            more = await get_more_results(
                query="test",
                offset=2,
                limit=2
            )
            
            self.test_results.append({
                "test": test_name,
                "status": "✅ PASS"
            })
            
        except Exception as e:
            self.test_results.append({
                "test": test_name,
                "status": "❌ FAIL",
                "error": str(e)
            })
    
    async def test_search_by_file(self):
        """Test search_by_file functionality."""
        test_name = "search_by_file"
        print(f"\n📝 Testing {test_name}...")
        
        test_files = [
            "/Users/test/project/main.py",
            "src/components/App.tsx",
            "README.md"
        ]
        
        for i, file_path in enumerate(test_files, 1):
            try:
                print(f"  [{i}/{len(test_files)}] Searching for file: {file_path}...")
                result = await search_by_file(FileSearchParams(
                    file_path=file_path,
                    limit=5
                ))
                
                self.test_results.append({
                    "test": f"{test_name}_{i}",
                    "status": "✅ PASS"
                })
                
            except Exception as e:
                self.test_results.append({
                    "test": f"{test_name}_{i}",
                    "status": "❌ FAIL",
                    "error": str(e)
                })
    
    async def test_search_by_concept(self):
        """Test search_by_concept functionality."""
        test_name = "search_by_concept"
        print(f"\n📝 Testing {test_name}...")
        
        concepts = [
            "security",
            "docker",
            "testing",
            "performance",
            "authentication"
        ]
        
        for i, concept in enumerate(concepts, 1):
            try:
                print(f"  [{i}/{len(concepts)}] Searching for concept: {concept}...")
                result = await search_by_concept(ConceptSearchParams(
                    concept=concept,
                    include_files=True,
                    limit=10
                ))
                
                self.test_results.append({
                    "test": f"{test_name}_{i}",
                    "status": "✅ PASS"
                })
                
            except Exception as e:
                self.test_results.append({
                    "test": f"{test_name}_{i}",
                    "status": "❌ FAIL",
                    "error": str(e)
                })
    
    async def test_edge_cases(self):
        """Test edge cases and error handling."""
        test_name = "edge_cases"
        print(f"\n📝 Testing {test_name}...")
        
        edge_cases = [
            # Empty query
            {
                "func": reflect_on_past,
                "params": {"query": ""},
                "description": "Empty query"
            },
            # Very long query
            {
                "func": reflect_on_past,
                "params": {"query": "a" * 10000},
                "description": "Very long query"
            },
            # Invalid project
            {
                "func": reflect_on_past,
                "params": {"query": "test", "project": "nonexistent-project-xyz"},
                "description": "Invalid project"
            },
            # Negative limit
            {
                "func": reflect_on_past,
                "params": {"query": "test", "limit": -1},
                "description": "Negative limit"
            },
            # Invalid min_score
            {
                "func": reflect_on_past,
                "params": {"query": "test", "min_score": 2.0},
                "description": "Invalid min_score > 1"
            }
        ]
        
        for i, test_case in enumerate(edge_cases, 1):
            try:
                print(f"  [{i}/{len(edge_cases)}] {test_case['description']}...")
                # These should handle gracefully or raise appropriate errors
                result = await test_case["func"](SearchParams(**test_case["params"]))
                
                self.test_results.append({
                    "test": f"{test_name}_{i}",
                    "status": "⚠️ HANDLED",
                    "description": test_case["description"]
                })
                
            except Exception as e:
                # Expected to fail, but should fail gracefully
                self.test_results.append({
                    "test": f"{test_name}_{i}",
                    "status": "✅ PROPERLY FAILED",
                    "error": str(e)[:100],
                    "description": test_case["description"]
                })
    
    def print_results(self):
        """Print test results summary."""
        print("\n" + "="*60)
        print("📊 TEST RESULTS SUMMARY")
        print("="*60)
        
        passed = sum(1 for r in self.test_results if "PASS" in r["status"])
        failed = sum(1 for r in self.test_results if "FAIL" in r["status"])
        handled = sum(1 for r in self.test_results if "HANDLED" in r["status"] or "PROPERLY FAILED" in r["status"])
        
        print(f"\n✅ Passed: {passed}")
        print(f"❌ Failed: {failed}")
        print(f"⚠️  Handled/Expected Failures: {handled}")
        print(f"📝 Total: {len(self.test_results)}")
        
        if failed > 0:
            print("\n❌ FAILED TESTS:")
            for result in self.test_results:
                if "FAIL" in result["status"] and "PROPERLY" not in result["status"]:
                    print(f"  - {result['test']}: {result.get('error', 'Unknown error')}")
        
        print("\n" + "="*60)
        return failed == 0
    
    async def run_all_tests(self):
        """Run all MCP tool tests."""
        print("🚀 Starting Comprehensive MCP Tool Tests")
        print("="*60)
        
        await self.setup()
        
        # Run all test suites
        await self.test_reflect_on_past()
        await self.test_store_reflection()
        await self.test_quick_search()
        await self.test_search_summary()
        await self.test_get_more_results()
        await self.test_search_by_file()
        await self.test_search_by_concept()
        await self.test_edge_cases()
        
        return self.print_results()

async def main():
    """Main test runner."""
    tester = TestMCPTools()
    success = await tester.run_all_tests()
    sys.exit(0 if success else 1)

if __name__ == "__main__":
    asyncio.run(main())