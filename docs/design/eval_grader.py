#!/usr/bin/env python3
"""
Three-Tier Evaluation Grader for Code Generation Sessions
Based on Anthropic's building_evals.ipynb patterns

Tier 1: Deterministic (Free, Always Run)
- Parse build outputs from bash tool_results
- Extract test results from pytest/unittest
- Calculate AST-GREP code quality scores
- Security pattern detection

Tier 2: Model-Based (Claude Grades Claude)
- Only when Tier 1 inconclusive (<70% confidence)
- Semantic correctness evaluation
- Design quality assessment
- Uses Batch API for 50% cost savings

Tier 3: Human Review (Ground Truth)
- Manual labeling for dataset creation
- Novel/complex cases only
- Establishes calibration baseline
"""

import json
import re
from typing import Dict, List, Any, Optional, Tuple
from pathlib import Path
from datetime import datetime
import subprocess
import sys

# Add parent directory to path for imports
sys.path.append(str(Path(__file__).parent.parent.parent))
from scripts.quality.ast_grep_final_analyzer import FinalASTGrepAnalyzer


class EvalGrader:
    """
    Three-tier evaluation system for code generation sessions.
    Integrates with existing narrative structure.
    """

    def __init__(self):
        """Initialize grader with AST-GREP analyzer."""
        self.ast_analyzer = FinalASTGrepAnalyzer()
        self.tier1_cache = {}  # Cache deterministic results

    def grade_conversation(
        self,
        conversation: Dict[str, Any],
        extracted_events: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        Main entry point: Grade a conversation using three-tier system.

        Args:
            conversation: Full JSONL conversation data
            extracted_events: Output from extract_events_v3.py

        Returns:
            eval_results dict to add to conversation signature
        """
        # Always run Tier 1 (deterministic, free)
        tier1_results = self._run_tier1(conversation, extracted_events)

        # Decide if we need Tier 2
        confidence = tier1_results.get("confidence", 0.0)
        needs_tier2 = confidence < 0.70

        eval_results = {
            "eval_tier": "tier1",
            "eval_cost": 0.00,
            "timestamp": datetime.utcnow().isoformat(),
            **tier1_results
        }

        if needs_tier2:
            tier2_results = self._run_tier2(conversation, extracted_events, tier1_results)
            eval_results.update({
                "eval_tier": "tier1+tier2",
                "eval_cost": tier2_results.get("cost", 0.30),
                "tier2_grade": tier2_results.get("grade"),
                "tier2_reasoning": tier2_results.get("reasoning")
            })

        return eval_results

    def _run_tier1(
        self,
        conversation: Dict[str, Any],
        extracted_events: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        Tier 1: Deterministic grading (code-based, from Anthropic's cookbook).

        Extracts from existing conversation data:
        - Build success/failure
        - Test pass/fail counts
        - AST-GREP code quality scores
        - Security issues
        - Error patterns
        """
        results = {
            "functional_correctness": 0.0,
            "code_quality": 0.0,
            "build_success": None,
            "test_results": {},
            "security_issues": 0,
            "confidence": 0.0,
            "grading_method": "deterministic"
        }

        # Parse build outputs
        build_data = self._parse_build_outputs(conversation)
        results["build_success"] = build_data.get("success")
        results["build_errors"] = build_data.get("errors", [])

        # Parse test results
        test_data = self._parse_test_results(conversation)
        results["test_results"] = test_data
        if test_data:
            passed = test_data.get("passed", 0)
            failed = test_data.get("failed", 0)
            total = passed + failed
            results["functional_correctness"] = passed / total if total > 0 else 0.0

        # Calculate AST-GREP scores for edited files
        quality_scores = self._calculate_code_quality(extracted_events)
        results["code_quality"] = quality_scores.get("score", 0.0)
        results["security_issues"] = quality_scores.get("security_issues", 0)
        results["ast_grep_details"] = quality_scores.get("details", {})

        # Calculate confidence based on available signals
        confidence_signals = []
        if results["build_success"] is not None:
            confidence_signals.append(0.3)
        if test_data:
            confidence_signals.append(0.4)
        if quality_scores.get("files_analyzed", 0) > 0:
            confidence_signals.append(0.3)

        results["confidence"] = sum(confidence_signals)

        # Overall score (weighted combination)
        if results["confidence"] >= 0.7:
            # Build: 40%, Tests: 40%, Quality: 20%
            results["overall_score"] = (
                (1.0 if results["build_success"] else 0.0) * 0.4 +
                results["functional_correctness"] * 0.4 +
                results["code_quality"] * 0.2
            )

        return results

    def _parse_build_outputs(self, conversation: Dict[str, Any]) -> Dict[str, Any]:
        """
        Extract build success/failure from bash tool_results.

        Looks for patterns like:
        - "compiled successfully"
        - "build failed"
        - "error" in build output
        """
        build_data = {"success": None, "errors": []}

        for msg in conversation:
            msg_data = msg.get("message", msg)
            content = msg_data.get("content", [])

            if not isinstance(content, list):
                continue

            for item in content:
                if not isinstance(item, dict):
                    continue

                # Look for tool_result from Bash
                if item.get("type") == "tool_result":
                    content_text = str(item.get("content", "")).lower()

                    # Build success indicators
                    if any(pattern in content_text for pattern in [
                        "compiled successfully",
                        "build succeeded",
                        "built successfully"
                    ]):
                        build_data["success"] = True

                    # Build failure indicators
                    if any(pattern in content_text for pattern in [
                        "build failed",
                        "compilation error",
                        "failed to compile"
                    ]):
                        build_data["success"] = False
                        # Extract error message
                        error_lines = [
                            line for line in content_text.split('\n')
                            if 'error' in line
                        ]
                        build_data["errors"].extend(error_lines[:3])  # First 3 errors

        return build_data

    def _parse_test_results(self, conversation: Dict[str, Any]) -> Dict[str, Any]:
        """
        Extract test results from bash tool_results.

        Looks for patterns from:
        - pytest: "10 passed, 2 failed"
        - unittest: "Ran 15 tests, 2 failures"
        - npm test: "Tests: 8 passed, 1 failed"
        """
        test_data = {}

        for msg in conversation:
            msg_data = msg.get("message", msg)
            content = msg_data.get("content", [])

            if not isinstance(content, list):
                continue

            for item in content:
                if not isinstance(item, dict):
                    continue

                if item.get("type") == "tool_result":
                    content_text = str(item.get("content", ""))

                    # pytest pattern
                    pytest_match = re.search(
                        r'(\d+) passed(?:, (\d+) failed)?',
                        content_text
                    )
                    if pytest_match:
                        test_data["passed"] = int(pytest_match.group(1))
                        test_data["failed"] = int(pytest_match.group(2) or 0)
                        test_data["framework"] = "pytest"

                    # unittest pattern
                    unittest_match = re.search(
                        r'Ran (\d+) tests.*?(\d+) failure',
                        content_text
                    )
                    if unittest_match:
                        total = int(unittest_match.group(1))
                        failed = int(unittest_match.group(2))
                        test_data["passed"] = total - failed
                        test_data["failed"] = failed
                        test_data["framework"] = "unittest"

                    # npm test pattern
                    npm_match = re.search(
                        r'Tests:\s*(\d+) passed.*?(\d+) failed',
                        content_text
                    )
                    if npm_match:
                        test_data["passed"] = int(npm_match.group(1))
                        test_data["failed"] = int(npm_match.group(2))
                        test_data["framework"] = "jest/npm"

        return test_data

    def _calculate_code_quality(
        self,
        extracted_events: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        Calculate AST-GREP code quality scores for edited files.

        Uses existing FinalASTGrepAnalyzer to score files mentioned
        in the Solution Pattern section.
        """
        quality_data = {
            "score": 0.0,
            "security_issues": 0,
            "files_analyzed": 0,
            "details": {}
        }

        # Get files from solution pattern
        solution_pattern = extracted_events.get("solution_pattern", [])
        files_to_analyze = []

        for pattern in solution_pattern:
            file_path = pattern.get("file", "")
            if file_path and file_path != "unknown":
                files_to_analyze.append(file_path)

        if not files_to_analyze:
            return quality_data

        # Analyze each file with AST-GREP
        total_score = 0.0
        total_security = 0

        for file_path in files_to_analyze:
            try:
                # Convert to absolute path if relative
                abs_path = Path(file_path)
                if not abs_path.exists():
                    continue

                # Run AST-GREP analysis
                result = self.ast_analyzer.analyze_file(str(abs_path))

                # Extract scores
                good_count = result.get("summary", {}).get("good_patterns", 0)
                bad_count = result.get("summary", {}).get("bad_patterns", 0)

                # Simple scoring: good patterns add, bad patterns subtract
                file_score = max(0, min(100, 50 + good_count * 5 - bad_count * 10)) / 100.0

                total_score += file_score
                quality_data["files_analyzed"] += 1

                # Count security issues
                security_categories = ["python_security", "ts_security", "sql_injection"]
                for category in security_categories:
                    if category in result.get("matches_by_category", {}):
                        total_security += len(result["matches_by_category"][category])

                quality_data["details"][file_path] = {
                    "score": file_score,
                    "good_patterns": good_count,
                    "bad_patterns": bad_count
                }

            except Exception as e:
                # File might not exist or be inaccessible
                continue

        # Average score across files
        if quality_data["files_analyzed"] > 0:
            quality_data["score"] = total_score / quality_data["files_analyzed"]
            quality_data["security_issues"] = total_security

        return quality_data

    def _run_tier2(
        self,
        conversation: Dict[str, Any],
        extracted_events: Dict[str, Any],
        tier1_results: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        Tier 2: Model-based grading (Claude grades Claude).

        Only called when Tier 1 confidence < 70%.
        Uses grader prompt similar to Anthropic's cookbook example.

        TODO: Implement once GRADER_PROMPT.md is created
        """
        return {
            "grade": 0.0,
            "reasoning": "Tier 2 not yet implemented",
            "cost": 0.0
        }


def main():
    """
    Example usage and testing.
    """
    grader = EvalGrader()

    # Test with sample conversation
    sample_conversation = []  # Would load from JSONL
    sample_events = {}  # Would get from extract_events_v3.py

    results = grader.grade_conversation(sample_conversation, sample_events)
    print(json.dumps(results, indent=2))


if __name__ == "__main__":
    main()
