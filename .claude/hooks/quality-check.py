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
                # Format the feedback
                feedback = f"Quality analysis for {Path(file_path).name}:\n\n"

                # Extract relevant lines from output
                relevant_lines = []
                for line in output.split('\n'):
                    if any(indicator in line for indicator in ['Issue', 'Warning', 'Error', 'Score', '🔴', '🟠', '🟡']):
                        relevant_lines.append(line)

                if relevant_lines:
                    feedback += '\n'.join(relevant_lines[:10])  # First 10 issue lines
                else:
                    feedback += output[:500]  # First 500 chars if no specific issues found

                feedback += "\n\n⚠️ Please review and fix the quality issues in the code you just wrote."

                # Exit code 2 with stderr makes Claude see the feedback
                # Also print to stdout for visibility
                print(feedback)
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