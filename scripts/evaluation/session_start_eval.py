#!/usr/bin/env python3
"""
Session-start evaluation for claude-self-reflect.
Quick health check of MCP tools, search quality, and performance.

Usage:
    python session_start_eval.py --quick    # 5 tests, <30s
    python session_start_eval.py            # All 20 tests
    python session_start_eval.py --json     # JSON output
"""

import asyncio
import json
import time
import sys
import os
import argparse
from pathlib import Path
from dataclasses import dataclass, asdict
from typing import List, Dict, Any, Optional
from datetime import datetime

# Try importing rich for beautiful output
try:
    from rich.console import Console
    from rich.table import Table
    from rich.panel import Panel
    from rich.progress import Progress, SpinnerColumn, TextColumn
    HAS_RICH = True
    console = Console()
except ImportError:
    HAS_RICH = False
    console = None

# Add MCP server to path
mcp_src = str(Path(__file__).parent.parent.parent / "mcp-server/src")
sys.path.insert(0, mcp_src)
os.chdir(mcp_src)  # Change to src directory for relative imports

# Import MCP components
try:
    import search_tools
    import temporal_tools
    import reflection_tools
    import embedding_manager
    import project_resolver
    import app_context
    MCP_AVAILABLE = True
except ImportError as e:
    MCP_AVAILABLE = False
    MCP_IMPORT_ERROR = str(e)


@dataclass
class TestResult:
    """Result of a single test"""
    test_id: str
    status: str  # "pass", "fail", "skip"
    duration_ms: int
    message: str
    score: Optional[float] = None
    error: Optional[str] = None


@dataclass
class EvalSummary:
    """Summary of evaluation run"""
    total_tests: int
    passed: int
    failed: int
    skipped: int
    total_duration_ms: int
    results: List[TestResult]
    status: str  # "HEALTHY", "DEGRADED", "FAILED"
    timestamp: str


class SessionStartEvaluator:
    """Lightweight evaluator for session-start health checks"""

    def __init__(self, timeout_seconds: int = 30):
        self.timeout = timeout_seconds
        self.results: List[TestResult] = []
        self.tasks = self._load_tasks()

        # Initialize MCP components if available
        if MCP_AVAILABLE:
            try:
                self.embedding_manager = embedding_manager.EmbeddingManager()
                self.project_resolver = project_resolver.ProjectResolver()
                self.app_context = app_context.AppContext(
                    embedding_manager=self.embedding_manager,
                    project_resolver=self.project_resolver
                )
                self.search_tools = search_tools.SearchTools(self.app_context)
                self.temporal_tools = temporal_tools.TemporalTools(self.app_context)
                self.reflection_tools = reflection_tools.ReflectionTools(self.app_context)
                self.mcp_initialized = True
            except Exception as e:
                self.mcp_initialized = False
                self.mcp_error = str(e)
        else:
            self.mcp_initialized = False
            self.mcp_error = MCP_IMPORT_ERROR if not MCP_AVAILABLE else None

    def _load_tasks(self) -> Dict[str, Any]:
        """Load evaluation tasks from JSON file"""
        task_file = Path(__file__).parent / "evaluation_tasks.json"
        if not task_file.exists():
            return {"quick_tests": [], "tasks": []}

        with open(task_file) as f:
            return json.load(f)

    async def _test_qdrant_connectivity(self) -> TestResult:
        """Test 1: Qdrant connectivity"""
        start = time.time()

        try:
            # Simple connectivity check
            import requests
            response = requests.get("http://localhost:6333/", timeout=5)
            duration_ms = int((time.time() - start) * 1000)

            if response.status_code == 200:
                return TestResult(
                    test_id="qdrant_connectivity",
                    status="pass",
                    duration_ms=duration_ms,
                    message="Qdrant responding"
                )
            else:
                return TestResult(
                    test_id="qdrant_connectivity",
                    status="fail",
                    duration_ms=duration_ms,
                    message=f"Qdrant returned status {response.status_code}"
                )
        except Exception as e:
            duration_ms = int((time.time() - start) * 1000)
            return TestResult(
                test_id="qdrant_connectivity",
                status="fail",
                duration_ms=duration_ms,
                message="Qdrant not accessible",
                error=str(e)
            )

    async def _test_search_accuracy(self) -> TestResult:
        """Test 2: Search accuracy with known query"""
        start = time.time()

        if not self.mcp_initialized:
            return TestResult(
                test_id="search_accuracy",
                status="skip",
                duration_ms=0,
                message="MCP not initialized",
                error=self.mcp_error
            )

        try:
            # Mock context
            class MockContext:
                async def debug(self, msg): pass

            ctx = MockContext()

            # Search for "docker" - common term likely in test data
            result = await self.search_tools.csr_reflect_on_past(
                ctx=ctx,
                query="docker",
                limit=3,
                min_score=0.3
            )

            duration_ms = int((time.time() - start) * 1000)

            # Check if we got results
            if result and "docker" in result.lower():
                return TestResult(
                    test_id="search_accuracy",
                    status="pass",
                    duration_ms=duration_ms,
                    message="Search returned relevant results",
                    score=0.7  # Placeholder score
                )
            else:
                return TestResult(
                    test_id="search_accuracy",
                    status="fail",
                    duration_ms=duration_ms,
                    message="Search returned no/irrelevant results"
                )
        except Exception as e:
            duration_ms = int((time.time() - start) * 1000)
            return TestResult(
                test_id="search_accuracy",
                status="fail",
                duration_ms=duration_ms,
                message="Search failed",
                error=str(e)
            )

    async def _test_performance(self) -> TestResult:
        """Test 3: Performance target (<500ms)"""
        start = time.time()

        if not self.mcp_initialized:
            return TestResult(
                test_id="performance",
                status="skip",
                duration_ms=0,
                message="MCP not initialized"
            )

        try:
            class MockContext:
                async def debug(self, msg): pass

            ctx = MockContext()

            # Run quick search 3 times
            durations = []
            for _ in range(3):
                test_start = time.time()
                await self.search_tools.csr_quick_check(
                    ctx=ctx,
                    query="test",
                    min_score=0.3
                )
                durations.append(int((time.time() - test_start) * 1000))

            avg_duration = sum(durations) / len(durations)
            total_duration = int((time.time() - start) * 1000)

            target_ms = int(os.getenv('EVAL_PERFORMANCE_TARGET_MS', '500'))

            if avg_duration < target_ms:
                return TestResult(
                    test_id="performance",
                    status="pass",
                    duration_ms=total_duration,
                    message=f"Avg: {avg_duration:.0f}ms (target: <{target_ms}ms)",
                    score=avg_duration
                )
            else:
                return TestResult(
                    test_id="performance",
                    status="fail",
                    duration_ms=total_duration,
                    message=f"Avg: {avg_duration:.0f}ms exceeds target {target_ms}ms",
                    score=avg_duration
                )
        except Exception as e:
            duration_ms = int((time.time() - start) * 1000)
            return TestResult(
                test_id="performance",
                status="fail",
                duration_ms=duration_ms,
                message="Performance test failed",
                error=str(e)
            )

    async def _test_token_efficiency(self) -> TestResult:
        """Test 4: Token efficiency (brief mode)"""
        start = time.time()

        if not self.mcp_initialized:
            return TestResult(
                test_id="token_efficiency",
                status="skip",
                duration_ms=0,
                message="MCP not initialized"
            )

        try:
            class MockContext:
                async def debug(self, msg): pass

            ctx = MockContext()

            # Compare full vs brief mode
            result_full = await self.search_tools.csr_reflect_on_past(
                ctx=ctx,
                query="testing",
                limit=3,
                brief=False
            )

            result_brief = await self.search_tools.csr_reflect_on_past(
                ctx=ctx,
                query="testing",
                limit=3,
                brief=True
            )

            duration_ms = int((time.time() - start) * 1000)

            # Rough token estimation (chars / 4)
            tokens_full = len(result_full) / 4
            tokens_brief = len(result_brief) / 4
            reduction = (1 - tokens_brief/tokens_full) * 100 if tokens_full > 0 else 0

            if reduction > 30:  # At least 30% reduction
                return TestResult(
                    test_id="token_efficiency",
                    status="pass",
                    duration_ms=duration_ms,
                    message=f"{reduction:.0f}% token reduction in brief mode",
                    score=reduction
                )
            else:
                return TestResult(
                    test_id="token_efficiency",
                    status="fail",
                    duration_ms=duration_ms,
                    message=f"Only {reduction:.0f}% reduction (target: >30%)",
                    score=reduction
                )
        except Exception as e:
            duration_ms = int((time.time() - start) * 1000)
            return TestResult(
                test_id="token_efficiency",
                status="fail",
                duration_ms=duration_ms,
                message="Token efficiency test failed",
                error=str(e)
            )

    async def _test_tool_availability(self) -> TestResult:
        """Test 5: All MCP tools available"""
        start = time.time()

        if not self.mcp_initialized:
            return TestResult(
                test_id="tool_availability",
                status="fail",
                duration_ms=0,
                message="MCP components not available",
                error=self.mcp_error
            )

        duration_ms = int((time.time() - start) * 1000)

        # Check if key tools are accessible
        tools_available = (
            hasattr(self, 'search_tools') and
            hasattr(self, 'temporal_tools') and
            hasattr(self, 'reflection_tools')
        )

        if tools_available:
            return TestResult(
                test_id="tool_availability",
                status="pass",
                duration_ms=duration_ms,
                message="All MCP tool classes accessible"
            )
        else:
            return TestResult(
                test_id="tool_availability",
                status="fail",
                duration_ms=duration_ms,
                message="Some MCP tools not accessible"
            )

    async def run_quick_checks(self) -> EvalSummary:
        """Run 5 critical tests for session start (<30s)"""
        start_time = time.time()
        self.results = []

        # Run quick tests
        tests = [
            self._test_qdrant_connectivity(),
            self._test_search_accuracy(),
            self._test_performance(),
            self._test_token_efficiency(),
            self._test_tool_availability()
        ]

        # Run with timeout
        try:
            self.results = await asyncio.wait_for(
                asyncio.gather(*tests, return_exceptions=True),
                timeout=self.timeout
            )
        except asyncio.TimeoutError:
            # Some tests didn't complete
            self.results = [r for r in self.results if isinstance(r, TestResult)]
            self.results.append(TestResult(
                test_id="timeout",
                status="fail",
                duration_ms=self.timeout * 1000,
                message=f"Evaluation exceeded {self.timeout}s timeout"
            ))

        # Handle any exceptions
        self.results = [
            r if isinstance(r, TestResult) else TestResult(
                test_id="error",
                status="fail",
                duration_ms=0,
                message="Test raised exception",
                error=str(r)
            )
            for r in self.results
        ]

        total_duration = int((time.time() - start_time) * 1000)

        # Calculate summary
        passed = sum(1 for r in self.results if r.status == "pass")
        failed = sum(1 for r in self.results if r.status == "fail")
        skipped = sum(1 for r in self.results if r.status == "skip")

        # Determine overall status
        if failed == 0:
            status = "HEALTHY"
        elif passed >= len(self.results) * 0.6:  # 60% passing
            status = "DEGRADED"
        else:
            status = "FAILED"

        return EvalSummary(
            total_tests=len(self.results),
            passed=passed,
            failed=failed,
            skipped=skipped,
            total_duration_ms=total_duration,
            results=self.results,
            status=status,
            timestamp=datetime.now().isoformat()
        )

    def print_banner(self, summary: EvalSummary):
        """Display visual banner with results"""
        if HAS_RICH and console:
            self._print_rich_banner(summary)
        else:
            self._print_simple_banner(summary)

    def _print_rich_banner(self, summary: EvalSummary):
        """Rich-formatted banner"""
        # Title
        console.print("\n" + "━" * 70)
        console.print("🧪 [bold]Claude Self-Reflect Health Check[/bold]")
        console.print("━" * 70)

        # Results
        for result in summary.results:
            if result.status == "pass":
                icon = "✅"
                color = "green"
            elif result.status == "fail":
                icon = "❌"
                color = "red"
            else:
                icon = "⏭️ "
                color = "yellow"

            msg = result.message
            if result.score is not None and result.test_id == "performance":
                msg = f"({result.duration_ms}ms avg)"
            elif result.score is not None:
                msg = f"({msg})"

            console.print(
                f"{icon} [bold]{result.test_id.replace('_', ' ').title()}[/bold]"
                f"[{color}] ({result.duration_ms}ms) {msg}[/{color}]"
            )

            if result.error:
                console.print(f"   [dim]Error: {result.error[:60]}...[/dim]")

        # Summary
        console.print("\n" + "─" * 70)

        status_color = {
            "HEALTHY": "green",
            "DEGRADED": "yellow",
            "FAILED": "red"
        }[summary.status]

        console.print(
            f"📊 Overall: [{status_color}][bold]{summary.status}[/bold][/{status_color}] "
            f"({summary.passed}/{summary.total_tests} passed)"
        )
        console.print(f"⏱️  Total time: {summary.total_duration_ms/1000:.1f}s")

        # Recommendations
        if summary.failed > 0:
            console.print("\n💡 [yellow]Recommendations:[/yellow]")
            for result in summary.results:
                if result.status == "fail":
                    if "qdrant" in result.test_id.lower():
                        console.print("   - Start Qdrant: docker compose up -d qdrant")
                    elif "search" in result.test_id.lower():
                        console.print("   - Check collections: python mcp-server/src/status.py")
                    elif "performance" in result.test_id.lower():
                        console.print("   - Check Qdrant resources: docker stats qdrant")

        console.print("━" * 70 + "\n")

    def _print_simple_banner(self, summary: EvalSummary):
        """Simple text banner (no rich)"""
        print("\n" + "=" * 70)
        print("🧪 Claude Self-Reflect Health Check")
        print("=" * 70)

        for result in summary.results:
            icon = "✅" if result.status == "pass" else "❌" if result.status == "fail" else "⏭️"
            print(f"{icon} {result.test_id}: {result.message} ({result.duration_ms}ms)")
            if result.error:
                print(f"   Error: {result.error[:60]}...")

        print("\n" + "-" * 70)
        print(f"📊 Overall: {summary.status} ({summary.passed}/{summary.total_tests} passed)")
        print(f"⏱️  Total time: {summary.total_duration_ms/1000:.1f}s")
        print("=" * 70 + "\n")

    def to_json(self, summary: EvalSummary) -> str:
        """Export results as JSON"""
        return json.dumps(asdict(summary), indent=2)


async def main():
    """Main entry point"""
    parser = argparse.ArgumentParser(description="Session-start evaluation for claude-self-reflect")
    parser.add_argument('--quick', action='store_true', help='Run only quick checks (5 tests)')
    parser.add_argument('--json', action='store_true', help='Output JSON instead of banner')
    parser.add_argument('--silent', action='store_true', help='Suppress banner output')
    parser.add_argument('--timeout', type=int, default=30, help='Max execution time in seconds')

    args = parser.parse_args()

    # Create evaluator
    evaluator = SessionStartEvaluator(timeout_seconds=args.timeout)

    # Run evaluation
    if args.quick:
        summary = await evaluator.run_quick_checks()
    else:
        # For now, only quick checks are implemented
        # Full evaluation would run all 20 tasks from evaluation_tasks.json
        summary = await evaluator.run_quick_checks()

    # Output results
    if args.json:
        print(evaluator.to_json(summary))
    elif not args.silent:
        evaluator.print_banner(summary)

    # Exit code
    sys.exit(0 if summary.status in ["HEALTHY", "DEGRADED"] else 1)


if __name__ == "__main__":
    asyncio.run(main())
