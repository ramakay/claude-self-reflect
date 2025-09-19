#!/usr/bin/env python3
"""
Quality check hook for Claude Code PostToolUse events.
Analyzes code quality after file edits and provides feedback to Claude.
"""
import json
import sys
import subprocess
from pathlib import Path

def main():
    try:
        # Read hook input from stdin
        input_data = json.load(sys.stdin)

        # Only process Edit/Write/MultiEdit tools
        tool_name = input_data.get("tool_name", "")
        if tool_name not in ["Edit", "Write", "MultiEdit", "NotebookEdit"]:
            sys.exit(0)

        # Get the edited file path
        tool_input = input_data.get("tool_input", {})
        file_path = tool_input.get("file_path", "")

        if not file_path:
            sys.exit(0)

        # Only check code files
        valid_extensions = ['.py', '.ts', '.js', '.tsx', '.jsx']
        if not any(file_path.endswith(ext) for ext in valid_extensions):
            sys.exit(0)

        # Find project root
        project_root = Path(__file__).parent.parent.parent
        analyzer_script = project_root / "scripts" / "ast_grep_final_analyzer.py"

        if not analyzer_script.exists():
            sys.exit(0)

        # Use venv Python if available
        venv_python = project_root / "venv" / "bin" / "python"
        python_cmd = str(venv_python) if venv_python.exists() else "python3"

        # Run quality analysis
        try:
            result = subprocess.run(
                [python_cmd, str(analyzer_script), file_path],
                capture_output=True,
                text=True,
                timeout=2
            )

            # Parse the output for issues
            output = result.stdout + result.stderr

            # Look for quality indicators
            has_critical = "🔴" in output or "critical" in output.lower()
            has_issues = "🟠" in output or "warning" in output.lower()

            # Extract quality score if present
            quality_score = None
            if "Quality Score:" in output:
                for line in output.split('\n'):
                    if "Quality Score:" in line:
                        try:
                            score_str = line.split(":")[-1].strip().rstrip('%')
                            quality_score = float(score_str)
                        except:
                            pass

            # If there are issues, report them to Claude
            if has_critical or has_issues or (quality_score and quality_score < 70):
                # Build formatted feedback like the examples show
                file_name = Path(file_path).name

                # Extract issue details
                issues_found = []
                if "print-call" in output:
                    for line in output.split('\n'):
                        if "print-call:" in line:
                            issues_found.append("• Replace print statements with logger")
                            break

                if "nested-if" in output:
                    issues_found.append("• Refactor deeply nested if statements")

                if "nested-loops" in output:
                    issues_found.append("• Optimize nested loops for performance")

                if "bare-except" in output or "broad-except" in output:
                    issues_found.append("• Use specific exception handlers")

                # Format the message like their examples
                feedback_parts = [
                    f"Code quality check failed for {file_name}:",
                    f"• Quality Score: {quality_score:.1f}% (threshold: 70%)"
                ]

                if issues_found:
                    feedback_parts.append("\nTop issues to fix:")
                    feedback_parts.extend(issues_found[:5])

                feedback_parts.append("\nPlease fix these quality issues before proceeding.")

                feedback = '\n'.join(feedback_parts)

                # Exit code 2 with stderr makes Claude see the feedback
                print(feedback, file=sys.stderr)
                sys.exit(2)

        except subprocess.TimeoutExpired:
            # Analysis took too long, skip
            pass
        except Exception as e:
            # Log error but don't block
            print(f"Quality check error: {e}", file=sys.stderr)

    except Exception as e:
        # Invalid JSON or other error, don't block
        pass

    # No issues or analysis succeeded
    sys.exit(0)

if __name__ == "__main__":
    main()