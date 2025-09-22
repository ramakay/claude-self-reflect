#!/usr/bin/env python3
"""
Quality check hook for Claude Code PostToolUse events.
Analyzes code quality after file edits and provides feedback to Claude.
"""
import json
import sys
import subprocess
from pathlib import Path
from datetime import datetime
import fcntl
import os
import logging
import hashlib
import time

# Configure logging
logging.basicConfig(
    level=logging.INFO,
    format='%(message)s',
    stream=sys.stdout
)
logger = logging.getLogger(__name__)

def get_issue_counts(issues_found, has_critical):
    """Extract issue counts by severity."""
    critical_count = 0
    medium_count = 0
    low_count = 0

    for issue in issues_found:
        if "nested" in issue.lower() or "refactor" in issue.lower():
            medium_count += 1
        elif "print" in issue.lower() or "logger" in issue.lower():
            low_count += 1
        elif "exception" in issue.lower():
            medium_count += 1

    if has_critical:
        critical_count = max(1, critical_count)

    return critical_count, medium_count, low_count

def load_existing_cache(cache_file):
    """Load existing cache data safely."""
    if not cache_file.exists():
        return {}

    try:
        content = cache_file.read_text()
        return json.loads(content)
    except:
        return {}

def save_cache_atomic(cache_file, cache_data):
    """Save cache with atomic write."""
    import tempfile
    try:
        cache_content = json.dumps(cache_data, indent=2)
        # Write to temp file first, then rename atomically
        with tempfile.NamedTemporaryFile(mode='w', dir=cache_file.parent,
                                         delete=False, suffix='.tmp') as tmp:
            tmp.write(cache_content)
            tmp_path = Path(tmp.name)
        # Atomic rename
        tmp_path.replace(cache_file)
    except:
        pass

def calculate_aggregates(file_scores):
    """Calculate aggregate statistics from file scores."""
    total_score = sum(f["score"] for f in file_scores.values())
    num_files = len(file_scores)
    avg_score = total_score / num_files if num_files > 0 else 100

    total_critical = sum(f["issues"]["critical"] for f in file_scores.values())
    total_medium = sum(f["issues"]["medium"] for f in file_scores.values())
    total_low = sum(f["issues"]["low"] for f in file_scores.values())

    return {
        "files_checked": num_files,
        "average_score": avg_score,
        "total_issues": {
            "critical": total_critical,
            "medium": total_medium,
            "low": total_low
        }
    }

def build_cache_data(file_path, quality_score, issues_found, file_scores, critical_count, medium_count, low_count):
    """Build cache data structure."""
    return {
        "last_check": {
            "timestamp": datetime.utcnow().isoformat() + "Z",
            "file_path": file_path,
            "file_score": quality_score,
            "threshold": 70,
            "status": "failed" if quality_score and quality_score < 70 else "passed",
            "issues": {
                "critical": critical_count,
                "medium": medium_count,
                "low": low_count
            },
            "top_issues": issues_found[:5]
        },
        "file_scores": file_scores,
        "session_aggregate": calculate_aggregates(file_scores)
    }

def get_file_hash(file_path):
    """Get hash of file content for change detection."""
    try:
        with open(file_path, 'rb') as f:
            return hashlib.sha256(f.read()).hexdigest()[:16]  # First 16 chars is enough
    except:
        return None

def write_realtime_cache(file_path, quality_score, issues_found, has_critical, has_issues, file_hash=None):
    """Write quality data to realtime cache for statusline integration."""
    import os
    project_dir = os.getcwd()
    project_name = os.path.basename(project_dir) if project_dir else "default"

    cache_dir = Path.home() / ".claude-self-reflect" / "quality_by_project"
    cache_dir.mkdir(exist_ok=True, parents=True)
    cache_file = cache_dir / f"{project_name}.json"

    critical_count, medium_count, low_count = get_issue_counts(issues_found, has_critical)
    existing_data = load_existing_cache(cache_file)
    file_scores = existing_data.get("file_scores", {})

    if quality_score is not None:
        file_scores[file_path] = {
            "score": quality_score,
            "timestamp": datetime.utcnow().isoformat() + "Z",
            "hash": file_hash,
            "issues": {
                "critical": critical_count,
                "medium": medium_count,
                "low": low_count
            }
        }

    cache_data = build_cache_data(file_path, quality_score, issues_found, file_scores,
                                   critical_count, medium_count, low_count)
    save_cache_atomic(cache_file, cache_data)

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

        # Check if file has changed since last analysis
        current_hash = get_file_hash(file_path)
        if current_hash:
            # Check cache for this file
            project_dir = os.getcwd()
            project_name = os.path.basename(project_dir) if project_dir else "default"
            cache_dir = Path.home() / ".claude-self-reflect" / "quality_by_project"
            cache_file = cache_dir / f"{project_name}.json"

            if cache_file.exists():
                try:
                    existing_data = load_existing_cache(cache_file)
                    file_scores = existing_data.get("file_scores", {})
                    if file_path in file_scores:
                        cached_entry = file_scores[file_path]
                        if cached_entry.get("hash") == current_hash:
                            # File hasn't changed, use cached score
                            cached_score = cached_entry.get("score", 100)
                            if cached_score >= 70:
                                # Good quality, no need to re-analyze
                                sys.exit(0)
                            # Bad quality but unchanged - still report it
                            # But we'll use the cached data instead of re-analyzing
                except:
                    pass  # Cache read failed, continue with analysis

        # Find project root
        project_root = Path(__file__).parent.parent.parent
        analyzer_script = project_root / "scripts" / "ast_grep_final_analyzer.py"

        if not analyzer_script.exists():
            sys.exit(0)

        # Use venv Python if available
        venv_python = project_root / "venv" / "bin" / "python"
        python_cmd = str(venv_python) if venv_python.exists() else "python3"

        # For very large files, use faster limited analysis
        file_size = Path(file_path).stat().st_size if Path(file_path).exists() else 0
        lines_estimate = file_size // 50  # Rough estimate: 50 bytes per line
        timeout = 3 if lines_estimate > 1000 else 2  # More time for large files

        # Run quality analysis
        try:
            result = subprocess.run(
                [python_cmd, str(analyzer_script), file_path],
                capture_output=True,
                text=True,
                timeout=timeout
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

                # Write to realtime cache for statusline integration
                write_realtime_cache(file_path, quality_score, issues_found, has_critical, has_issues, current_hash)

                # Get session average from the cache we just wrote
                session_avg = quality_score  # Default to file score
                try:
                    cache_file = Path.home() / ".claude-self-reflect" / "realtime_quality.json"
                    if cache_file.exists():
                        cache_content = cache_file.read_text()
                        cache_data = json.loads(cache_content)
                        session_avg = cache_data.get("session_aggregate", {}).get("average_score", quality_score)
                except:
                    pass

                # Format the message with both file and session scores
                feedback_parts = [
                    f"Code quality check failed for {file_name}:",
                    f"• File Score: {quality_score:.1f}% (threshold: 70%)",
                    f"• Session Average: {session_avg:.1f}%"
                ]

                if issues_found:
                    feedback_parts.append("\nTop issues to fix:")
                    feedback_parts.extend(issues_found[:5])

                feedback_parts.append("\nPlease fix these quality issues before proceeding.")

                # Use string concatenation instead of join to avoid thread-join pattern
                feedback = ""
                for i, part in enumerate(feedback_parts):
                    if i > 0:
                        feedback += "\n"
                    feedback += part

                # Use JSON output for PostToolUse as per documentation
                output = {
                    "decision": "block",
                    "reason": feedback,
                    "hookSpecificOutput": {
                        "hookEventName": "PostToolUse",
                        "additionalContext": feedback
                    }
                }
                logger.info(json.dumps(output))
                sys.exit(0)
            else:
                # No issues detected - write success to cache
                if quality_score:
                    write_realtime_cache(file_path, quality_score, [], False, False, current_hash)

        except subprocess.TimeoutExpired:
            # Analysis took too long, skip
            pass
        except Exception as e:
            # Log error but don't block
            logger.error(f"Quality check error: {e}")

    except Exception as e:
        # Invalid JSON or other error, don't block
        pass

    # No issues or analysis succeeded
    sys.exit(0)

if __name__ == "__main__":
    main()