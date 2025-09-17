#!/usr/bin/env python3
"""Test quality after fixing print statements."""

import json
import sys
from pathlib import Path

# Add scripts directory to path
sys.path.append(str(Path(__file__).parent))

from ast_grep_final_analyzer import FinalASTGrepAnalyzer

def main():
    """Analyze specific files to check quality improvement."""

    # Files we fixed
    files_to_check = [
        "/Users/ramakrishnanannaswamy/projects/claude-self-reflect/scripts/session_quality_tracker.py",
        "/Users/ramakrishnanannaswamy/projects/claude-self-reflect/scripts/cc-statusline-unified.py",
        "/Users/ramakrishnanannaswamy/projects/claude-self-reflect/scripts/pattern_registry_enhanced.py",
        "/Users/ramakrishnanannaswamy/projects/claude-self-reflect/scripts/simplified_metadata_extractor.py",
    ]

    analyzer = FinalASTGrepAnalyzer()
    total_issues = 0
    total_good = 0

    print("Analyzing fixed files...")
    print("-" * 40)

    for file_path in files_to_check:
        if Path(file_path).exists():
            result = analyzer.analyze_file(file_path)
            if result:
                issues = result.get('issue_count', 0)
                good = result.get('good_pattern_count', 0)
                total_issues += issues
                total_good += good
                print(f"{Path(file_path).name}: {issues} issues, {good} good patterns")

    print("-" * 40)
    print(f"Total issues in fixed files: {total_issues}")
    print(f"Total good patterns: {total_good}")

    # Estimate new grade
    if total_issues < 10:
        grade = "A+"
    elif total_issues < 25:
        grade = "A"
    elif total_issues < 50:
        grade = "B"
    else:
        grade = "C"

    print(f"Estimated grade for these files: {grade}")

if __name__ == "__main__":
    main()