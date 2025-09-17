#!/usr/bin/env python3
"""
Minimal evaluation runner for claude-self-reflect MCP tools.
Tests how well Claude uses our search tools to solve real tasks.
"""

import asyncio
import json
import os
import sys
import time
from datetime import datetime
from pathlib import Path
from typing import Dict, List, Optional, Any
from dataclasses import dataclass, asdict
import anthropic
from rich.console import Console
from rich.table import Table
from rich.progress import track

# Add parent directory to path for imports
sys.path.append(str(Path(__file__).parent.parent.parent / "mcp-server/src"))

console = Console()

@dataclass
class EvaluationTask:
    """Single evaluation task"""
    id: str
    prompt: str
    expected_tools: List[str]  # Tools we expect to be called
    verify_response: Dict[str, Any]  # How to verify success
    description: str
    category: str  # "search", "reflection", "temporal", etc.

@dataclass
class EvaluationResult:
    """Result of running one evaluation"""
    task_id: str
    success: bool
    tools_called: List[str]
    response: str
    reasoning: str  # From Claude's chain-of-thought
    runtime_ms: int
    tokens_used: int
    error: Optional[str] = None

class MCPEvaluator:
    """Evaluates Claude's use of MCP tools"""

    def __init__(self, api_key: str):
        self.client = anthropic.Anthropic(api_key=api_key)
        self.results: List[EvaluationResult] = []

    def load_tasks(self, task_file: str) -> List[EvaluationTask]:
        """Load evaluation tasks from JSON file"""
        with open(task_file) as f:
            data = json.load(f)
        return [EvaluationTask(**task) for task in data["tasks"]]

    async def run_single_task(self, task: EvaluationTask) -> EvaluationResult:
        """Run a single evaluation task through Claude"""

        start_time = time.time()
        tools_called = []

        try:
            # System prompt instructs Claude to use tools and show reasoning
            system_prompt = """You are evaluating the claude-self-reflect MCP tools.
For each task, you should:
1. <reasoning>Think about which tools to use and why</reasoning>
2. Call the appropriate MCP tools to solve the task
3. <response>Provide your final answer based on the tool results</response>

Available tools:
- reflect_on_past: Search past conversations semantically
- quick_search: Fast search returning count and top result
- search_by_concept: Search for specific development concepts
- search_by_file: Find conversations about specific files
- search_by_recency: Time-constrained semantic search
- get_recent_work: Get recent conversations
- store_reflection: Store an insight for future reference
- get_full_conversation: Get complete conversation from ID

Always explain your tool selection in the reasoning block."""

            # Create conversation with Claude
            message = self.client.messages.create(
                model="claude-3-5-sonnet-20241022",
                max_tokens=2000,
                temperature=0,  # Deterministic for evaluation
                system=system_prompt,
                messages=[
                    {"role": "user", "content": task.prompt}
                ],
                # This is where we'd pass actual MCP tools in production
                # For now, we'll simulate tool calls
                tools=self._get_mcp_tool_definitions(),
                tool_choice="auto"
            )

            # Extract reasoning and response
            content = message.content[0].text if message.content else ""
            reasoning = self._extract_block(content, "reasoning")
            response = self._extract_block(content, "response")

            # Track which tools were called
            if hasattr(message, 'tool_calls'):
                tools_called = [call.name for call in message.tool_calls]

            # Verify the response
            success = self._verify_response(task, response, tools_called)

            runtime_ms = int((time.time() - start_time) * 1000)

            return EvaluationResult(
                task_id=task.id,
                success=success,
                tools_called=tools_called,
                response=response,
                reasoning=reasoning,
                runtime_ms=runtime_ms,
                tokens_used=message.usage.total_tokens if hasattr(message, 'usage') else 0
            )

        except Exception as e:
            return EvaluationResult(
                task_id=task.id,
                success=False,
                tools_called=[],
                response="",
                reasoning="",
                runtime_ms=int((time.time() - start_time) * 1000),
                tokens_used=0,
                error=str(e)
            )

    def _extract_block(self, content: str, tag: str) -> str:
        """Extract content between XML tags"""
        start = f"<{tag}>"
        end = f"</{tag}>"
        if start in content and end in content:
            return content.split(start)[1].split(end)[0].strip()
        return ""

    def _verify_response(self, task: EvaluationTask, response: str, tools_called: List[str]) -> bool:
        """Verify if the response meets expectations"""

        # Check if expected tools were called
        if task.expected_tools:
            for expected_tool in task.expected_tools:
                if expected_tool not in tools_called:
                    return False

        # Check response content
        verify = task.verify_response
        if "contains" in verify:
            for term in verify["contains"]:
                if term.lower() not in response.lower():
                    return False

        if "not_contains" in verify:
            for term in verify["not_contains"]:
                if term.lower() in response.lower():
                    return False

        if "min_length" in verify:
            if len(response) < verify["min_length"]:
                return False

        return True

    def _get_mcp_tool_definitions(self) -> list:
        """Get MCP tool definitions for Claude"""
        # In production, these would come from the actual MCP server
        # For now, return mock definitions
        return [
            {
                "name": "reflect_on_past",
                "description": "Search for relevant past conversations using semantic search",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "query": {"type": "string"},
                        "limit": {"type": "integer", "default": 5}
                    },
                    "required": ["query"]
                }
            },
            # Add other tool definitions...
        ]

    async def run_evaluation(self, tasks: List[EvaluationTask]) -> None:
        """Run all evaluation tasks"""

        console.print(f"\n[bold]Running {len(tasks)} evaluation tasks...[/bold]\n")

        for task in track(tasks, description="Evaluating..."):
            result = await self.run_single_task(task)
            self.results.append(result)

            # Show immediate feedback
            status = "✅" if result.success else "❌"
            console.print(f"{status} {task.id}: {task.description}")
            if result.error:
                console.print(f"   [red]Error: {result.error}[/red]")

    def analyze_results(self) -> Dict[str, Any]:
        """Analyze evaluation results"""

        total = len(self.results)
        successes = sum(1 for r in self.results if r.success)

        # Tool usage statistics
        tool_counts = {}
        for result in self.results:
            for tool in result.tools_called:
                tool_counts[tool] = tool_counts.get(tool, 0) + 1

        # Performance metrics
        avg_runtime = sum(r.runtime_ms for r in self.results) / total if total > 0 else 0
        avg_tokens = sum(r.tokens_used for r in self.results) / total if total > 0 else 0

        # Category breakdown
        category_stats = {}
        # (Would need task categories for this)

        return {
            "total_tasks": total,
            "successes": successes,
            "success_rate": (successes / total * 100) if total > 0 else 0,
            "tool_usage": tool_counts,
            "avg_runtime_ms": avg_runtime,
            "avg_tokens": avg_tokens,
            "failures": [r for r in self.results if not r.success]
        }

    def print_report(self) -> None:
        """Print evaluation report"""

        stats = self.analyze_results()

        # Summary table
        table = Table(title="Evaluation Results")
        table.add_column("Metric", style="cyan")
        table.add_column("Value", style="green")

        table.add_row("Total Tasks", str(stats["total_tasks"]))
        table.add_row("Successful", str(stats["successes"]))
        table.add_row("Success Rate", f"{stats['success_rate']:.1f}%")
        table.add_row("Avg Runtime", f"{stats['avg_runtime_ms']:.0f}ms")
        table.add_row("Avg Tokens", f"{stats['avg_tokens']:.0f}")

        console.print(table)

        # Tool usage
        if stats["tool_usage"]:
            console.print("\n[bold]Tool Usage:[/bold]")
            for tool, count in sorted(stats["tool_usage"].items(), key=lambda x: x[1], reverse=True):
                console.print(f"  {tool}: {count} calls")

        # Failures
        if stats["failures"]:
            console.print("\n[bold red]Failed Tasks:[/bold red]")
            for failure in stats["failures"]:
                console.print(f"  ❌ {failure.task_id}")
                if failure.error:
                    console.print(f"     Error: {failure.error}")
                if failure.reasoning:
                    console.print(f"     Reasoning: {failure.reasoning[:100]}...")

    def save_results(self, output_file: str) -> None:
        """Save detailed results to JSON"""

        data = {
            "timestamp": datetime.now().isoformat(),
            "summary": self.analyze_results(),
            "results": [asdict(r) for r in self.results]
        }

        with open(output_file, 'w') as f:
            json.dump(data, f, indent=2)

        console.print(f"\n[green]Results saved to {output_file}[/green]")


async def main():
    """Main evaluation runner"""

    # Check for API key
    api_key = os.environ.get("ANTHROPIC_API_KEY")
    if not api_key:
        console.print("[red]Error: ANTHROPIC_API_KEY not set[/red]")
        sys.exit(1)

    # Load tasks
    task_file = Path(__file__).parent / "evaluation_tasks.json"
    if not task_file.exists():
        console.print(f"[red]Error: {task_file} not found[/red]")
        console.print("Creating example task file...")
        create_example_tasks(task_file)
        console.print(f"[green]Created {task_file} - please review and run again[/green]")
        sys.exit(0)

    # Run evaluation
    evaluator = MCPEvaluator(api_key)
    tasks = evaluator.load_tasks(str(task_file))

    await evaluator.run_evaluation(tasks)

    # Print results
    evaluator.print_report()

    # Save detailed results
    timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
    output_file = Path(__file__).parent / f"results_{timestamp}.json"
    evaluator.save_results(str(output_file))


def create_example_tasks(task_file: Path) -> None:
    """Create example evaluation tasks"""

    example_tasks = {
        "version": "1.0",
        "tasks": [
            {
                "id": "search_docker_errors",
                "prompt": "Find all conversations about Docker errors from last week and tell me the main issues",
                "expected_tools": ["search_by_recency"],
                "verify_response": {
                    "contains": ["docker", "container"],
                    "min_length": 50
                },
                "description": "Search for Docker-related issues with time constraint",
                "category": "temporal_search"
            },
            {
                "id": "find_file_modifications",
                "prompt": "What files were modified when we implemented the hot reload feature?",
                "expected_tools": ["search_by_concept", "reflect_on_past"],
                "verify_response": {
                    "contains": ["reload", "file"],
                    "min_length": 30
                },
                "description": "Find files related to a specific feature",
                "category": "concept_search"
            },
            {
                "id": "store_insight",
                "prompt": "Store this insight: The hot reload feature uses importlib to dynamically reload Python modules without restarting the MCP server",
                "expected_tools": ["store_reflection"],
                "verify_response": {
                    "contains": ["stored", "saved", "reflection"],
                    "not_contains": ["error", "failed"]
                },
                "description": "Store a technical insight",
                "category": "reflection"
            },
            {
                "id": "recent_work_summary",
                "prompt": "What have we been working on in the last 3 days? Group by day",
                "expected_tools": ["get_recent_work"],
                "verify_response": {
                    "min_length": 100
                },
                "description": "Summarize recent work grouped by time",
                "category": "temporal"
            },
            {
                "id": "quick_relevance_check",
                "prompt": "Is there any past discussion about evaluation frameworks? Just tell me yes or no with a count",
                "expected_tools": ["quick_search"],
                "verify_response": {
                    "min_length": 10
                },
                "description": "Quick existence check for a topic",
                "category": "quick_search"
            }
        ]
    }

    with open(task_file, 'w') as f:
        json.dump(example_tasks, f, indent=2)


if __name__ == "__main__":
    asyncio.run(main())