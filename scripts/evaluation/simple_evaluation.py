#!/usr/bin/env python3
"""
Simple evaluation script that tests MCP tools directly.
This version actually calls your MCP server to test real functionality.
"""

import asyncio
import json
import time
from pathlib import Path
import sys
import os

# Add MCP server to path
mcp_src = str(Path(__file__).parent.parent.parent / "mcp-server/src")
sys.path.insert(0, mcp_src)
os.chdir(mcp_src)  # Change to src directory for relative imports

# Now imports will work with relative imports
import search_tools
import temporal_tools
import reflection_tools
import embedding_manager
import project_resolver
import app_context

class SimpleEvaluator:
    """Direct evaluation of MCP tools without Claude"""

    def __init__(self):
        # Initialize the actual MCP components
        self.embedding_manager = embedding_manager.EmbeddingManager()
        self.project_resolver = project_resolver.ProjectResolver()

        # Create shared context
        self.app_context = app_context.AppContext(
            embedding_manager=self.embedding_manager,
            project_resolver=self.project_resolver
        )

        # Initialize tool classes
        self.search_tools = search_tools.SearchTools(self.app_context)
        self.temporal_tools = temporal_tools.TemporalTools(self.app_context)
        self.reflection_tools = reflection_tools.ReflectionTools(self.app_context)

        self.results = []

    async def test_search_accuracy(self):
        """Test 1: Can we find relevant conversations?"""
        print("\n📝 Test 1: Search Accuracy")

        test_queries = [
            ("docker container errors", ["docker", "container"]),
            ("MCP tool implementation", ["mcp", "tool"]),
            ("hot reload feature", ["reload", "import"])
        ]

        for query, expected_terms in test_queries:
            start = time.time()

            # Mock context object
            class MockContext:
                async def debug(self, msg): pass

            ctx = MockContext()

            # Call the actual search tool
            result = await self.search_tools.reflect_on_past(
                ctx=ctx,
                query=query,
                limit=3,
                min_score=0.3
            )

            elapsed = (time.time() - start) * 1000

            # Check if expected terms appear in results
            success = all(term.lower() in result.lower() for term in expected_terms)

            print(f"  Query: '{query}'")
            print(f"  Success: {'✅' if success else '❌'}")
            print(f"  Time: {elapsed:.0f}ms")

            self.results.append({
                "test": "search_accuracy",
                "query": query,
                "success": success,
                "time_ms": elapsed
            })

    async def test_search_performance(self):
        """Test 2: Are searches fast enough?"""
        print("\n⚡ Test 2: Search Performance")

        target_ms = 500  # Target: under 500ms

        queries = ["testing", "error handling", "deployment"]

        for query in queries:
            start = time.time()

            class MockContext:
                async def debug(self, msg): pass

            ctx = MockContext()

            result = await self.search_tools.quick_search(
                ctx=ctx,
                query=query,
                min_score=0.3
            )

            elapsed = (time.time() - start) * 1000
            success = elapsed < target_ms

            print(f"  Query: '{query}'")
            print(f"  Time: {elapsed:.0f}ms (target: <{target_ms}ms)")
            print(f"  Status: {'✅' if success else '🐌 Too slow'}")

            self.results.append({
                "test": "performance",
                "query": query,
                "success": success,
                "time_ms": elapsed
            })

    async def test_tool_overlap(self):
        """Test 3: Do different search tools return different results?"""
        print("\n🔍 Test 3: Tool Differentiation")

        query = "docker"

        class MockContext:
            async def debug(self, msg): pass

        ctx = MockContext()

        # Test different search methods
        results = {}

        # Regular search
        result1 = await self.search_tools.reflect_on_past(
            ctx=ctx, query=query, limit=2
        )
        results["reflect_on_past"] = len(result1)

        # Quick search
        result2 = await self.search_tools.quick_search(
            ctx=ctx, query=query
        )
        results["quick_search"] = 1 if "count" in result2 else 0

        # Concept search
        result3 = await self.search_tools.search_by_concept(
            ctx=ctx, concept=query, limit=2
        )
        results["search_by_concept"] = len(result3) if isinstance(result3, list) else 1

        print(f"  Query: '{query}'")
        for tool, count in results.items():
            print(f"  {tool}: {count} results")

        # Check if tools are differentiated
        unique_behaviors = len(set(results.values())) > 1
        print(f"  Differentiation: {'✅ Tools behave differently' if unique_behaviors else '⚠️  Tools too similar'}")

    async def test_response_size(self):
        """Test 4: Are responses token-efficient?"""
        print("\n📊 Test 4: Token Efficiency")

        class MockContext:
            async def debug(self, msg): pass

        ctx = MockContext()

        # Test with brief mode
        result_full = await self.search_tools.reflect_on_past(
            ctx=ctx,
            query="testing",
            limit=5,
            brief=False
        )

        result_brief = await self.search_tools.reflect_on_past(
            ctx=ctx,
            query="testing",
            limit=5,
            brief=True
        )

        # Rough token estimation (chars / 4)
        tokens_full = len(result_full) / 4
        tokens_brief = len(result_brief) / 4
        reduction = (1 - tokens_brief/tokens_full) * 100 if tokens_full > 0 else 0

        print(f"  Full mode: ~{tokens_full:.0f} tokens")
        print(f"  Brief mode: ~{tokens_brief:.0f} tokens")
        print(f"  Reduction: {reduction:.0f}%")
        print(f"  Efficiency: {'✅' if reduction > 50 else '⚠️  Could be better'}")

        self.results.append({
            "test": "token_efficiency",
            "tokens_full": tokens_full,
            "tokens_brief": tokens_brief,
            "reduction_percent": reduction,
            "success": reduction > 50
        })

    def print_summary(self):
        """Print evaluation summary"""
        print("\n" + "="*50)
        print("EVALUATION SUMMARY")
        print("="*50)

        total_tests = len(self.results)
        passed = sum(1 for r in self.results if r.get("success", False))

        print(f"\nTotal Tests: {total_tests}")
        print(f"Passed: {passed}")
        print(f"Success Rate: {(passed/total_tests*100):.0f}%")

        # Performance summary
        perf_tests = [r for r in self.results if "time_ms" in r]
        if perf_tests:
            avg_time = sum(r["time_ms"] for r in perf_tests) / len(perf_tests)
            print(f"\nAverage Response Time: {avg_time:.0f}ms")

        # Recommendations
        print("\n📋 RECOMMENDATIONS:")

        if passed < total_tests * 0.8:
            print("  🔴 Critical issues detected - fix before production")

        slow_queries = [r for r in self.results if r.get("time_ms", 0) > 500]
        if slow_queries:
            print(f"  ⚠️  {len(slow_queries)} slow queries - consider optimization")

        token_test = next((r for r in self.results if r.get("test") == "token_efficiency"), None)
        if token_test and token_test.get("reduction_percent", 0) < 50:
            print("  💡 Token reduction insufficient - improve brief mode")

    async def run_all_tests(self):
        """Run all evaluation tests"""
        try:
            await self.test_search_accuracy()
            await self.test_search_performance()
            await self.test_tool_overlap()
            await self.test_response_size()

            self.print_summary()

            # Save results
            output_file = Path(__file__).parent / "evaluation_results.json"
            with open(output_file, 'w') as f:
                json.dump(self.results, f, indent=2)
            print(f"\n💾 Results saved to {output_file}")

        except Exception as e:
            print(f"\n❌ Evaluation failed: {e}")
            print("\nMake sure:")
            print("  1. Qdrant is running (docker ps | grep qdrant)")
            print("  2. Collections exist (check with scripts/check-collections.py)")
            print("  3. MCP server dependencies are installed")


async def main():
    evaluator = SimpleEvaluator()
    await evaluator.run_all_tests()


if __name__ == "__main__":
    print("🧪 Claude Self-Reflect Tool Evaluation")
    print("="*50)
    asyncio.run(main())