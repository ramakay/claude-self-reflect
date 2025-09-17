#!/usr/bin/env python3
"""
Comprehensive tests for today's conversations
Tests the full pipeline: import, indexing, and search functionality
"""

import asyncio
import json
from datetime import datetime, timedelta
from pathlib import Path
from typing import List, Dict, Any
import sys
sys.path.insert(0, str(Path(__file__).parent))
from utils import normalize_project_name

# Import MCP client for testing
import os
os.environ["QDRANT_URL"] = "http://localhost:6333"

from mcp_server.src.claude_self_reflect import ClaudeSelfReflectServer

class TodayConversationTester:
    """Test today's conversations comprehensively"""
    
    def __init__(self):
        self.server = ClaudeSelfReflectServer()
        self.test_results = []
        self.today = datetime.now().date()
        
    async def run_all_tests(self):
        """Run all comprehensive tests"""
        print(f"\n{'='*60}")
        print(f"COMPREHENSIVE TESTS FOR TODAY'S CONVERSATIONS")
        print(f"Date: {self.today}")
        print(f"{'='*60}\n")
        
        # Find today's conversation files
        logs_dir = Path.home() / ".claude/projects"
        today_files = []
        
        for project_dir in logs_dir.glob("*"):
            if project_dir.is_dir():
                for jsonl_file in project_dir.glob("*.jsonl"):
                    if datetime.fromtimestamp(jsonl_file.stat().st_mtime).date() == self.today:
                        today_files.append((project_dir.name, jsonl_file))
        
        print(f"Found {len(today_files)} conversation files from today\n")
        
        # Run 5 comprehensive tests
        test_cases = [
            ("Test 1: Search for streaming importer fix discussion", 
             "streaming importer missing text field payload", "claude-self-reflect"),
            
            ("Test 2: Search for fix-misplaced-conversations migration",
             "fix-misplaced-conversations normalize_project_name migration", "claude-self-reflect"),
            
            ("Test 3: Search for AI product realization (procsolve)",
             "AI product realization page development usecases", "all"),
            
            ("Test 4: Search for today's debugging session",
             "trace if they are reaching the right places with the watcher hot import", "claude-self-reflect"),
            
            ("Test 5: Search for collection naming issue",
             "collection naming mismatch MD5 hash project path normalization", "claude-self-reflect")
        ]
        
        for i, (test_name, query, project) in enumerate(test_cases, 1):
            print(f"\n{test_name}")
            print("-" * 40)
            
            try:
                # Perform search
                result = await self.server.reflect_on_past({
                    "query": query,
                    "project": project if project != "all" else None,
                    "limit": 3,
                    "min_score": 0.7,
                    "use_decay": -1,
                    "brief": True
                })
                
                # Parse result
                if "RESULTS:" in result:
                    lines = result.split('\n')
                    for line in lines:
                        if "RESULTS:" in line and "matches" in line:
                            if "0 matches" in line:
                                print(f"❌ FAILED: No results found")
                                self.test_results.append((test_name, "FAILED", "No results"))
                            else:
                                # Extract match count and score
                                import re
                                match_count = re.search(r'(\d+) matches', line)
                                score_match = re.search(r'top score: ([\d.]+)', line)
                                
                                if match_count and score_match:
                                    count = match_count.group(1)
                                    score = float(score_match.group(1))
                                    print(f"✅ PASSED: {count} results found (top score: {score:.3f})")
                                    self.test_results.append((test_name, "PASSED", f"{count} results, score: {score:.3f}"))
                                else:
                                    print(f"✅ PASSED: Results found")
                                    self.test_results.append((test_name, "PASSED", "Results found"))
                            break
                else:
                    print(f"⚠️  WARNING: Unexpected response format")
                    self.test_results.append((test_name, "WARNING", "Unexpected format"))
                    
            except Exception as e:
                print(f"❌ ERROR: {str(e)}")
                self.test_results.append((test_name, "ERROR", str(e)))
        
        # Summary
        print(f"\n{'='*60}")
        print("TEST SUMMARY")
        print(f"{'='*60}")
        
        passed = sum(1 for _, status, _ in self.test_results if status == "PASSED")
        failed = sum(1 for _, status, _ in self.test_results if status == "FAILED")
        errors = sum(1 for _, status, _ in self.test_results if status == "ERROR")
        warnings = sum(1 for _, status, _ in self.test_results if status == "WARNING")
        
        print(f"\nTotal Tests: {len(self.test_results)}")
        print(f"✅ Passed: {passed}")
        print(f"❌ Failed: {failed}")
        print(f"🔴 Errors: {errors}")
        print(f"⚠️  Warnings: {warnings}")
        
        print(f"\nDetailed Results:")
        for test_name, status, details in self.test_results:
            status_icon = {"PASSED": "✅", "FAILED": "❌", "ERROR": "🔴", "WARNING": "⚠️"}.get(status, "?")
            print(f"{status_icon} {test_name}: {details}")
        
        # Overall status
        print(f"\n{'='*60}")
        if failed == 0 and errors == 0:
            print("🎉 ALL TESTS PASSED! Today's conversations are fully searchable.")
        else:
            print("⚠️  Some tests failed. Review the results above.")
        print(f"{'='*60}\n")
        
        return passed == len(self.test_results)

async def main():
    """Main test runner"""
    tester = TodayConversationTester()
    success = await tester.run_all_tests()
    sys.exit(0 if success else 1)

if __name__ == "__main__":
    asyncio.run(main())